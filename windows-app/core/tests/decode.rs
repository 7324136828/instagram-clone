//! Decoding real files: the fixtures, pictures generated on the fly (turned by EXIF, very large,
//! transparent, animated) and damaged files. Windows Imaging Component is exercised with the
//! codecs every Windows has (JPEG, TIFF) and, when this computer has the HEIF codec, with the
//! HEIC fixture; without the codec those tests print a note and pass.
//!
//! Generated files live in a folder of their own below the temporary folder, which is removed
//! when the test ends.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use image::codecs::gif::GifEncoder;
use image::{DynamicImage, Frame, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use instagram_core::decode::{
    cover_within, decode_picture, decode_thumbnail, decode_with_wic, fit_within, probe_dimensions, DecodeErrorKind,
    DecodedImage, DEFAULT_THUMBNAIL_SIDE, MAX_TEXTURE_SIDE,
};
use instagram_core::messages;

/// What Windows Imaging Component answers when no installed codec knows the data.
const COMPONENT_NOT_FOUND: &str = "0x88982F50";

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(relative)
}

/// A folder for the files of one test, removed again when the test ends.
struct TempFolder {
    path: PathBuf,
}

impl TempFolder {
    fn new(label: &str) -> TempFolder {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "instagram-core-decode-{}-{}-{label}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("the temporary folder can be created");
        TempFolder { path }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, bytes).expect("the test file can be written");
        path
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn encode(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), format).expect("the test picture can be encoded");
    bytes
}

fn pixel(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
    let start = (y as usize * image.width as usize + x as usize) * 4;
    [image.rgba[start], image.rgba[start + 1], image.rgba[start + 2], image.rgba[start + 3]]
}

fn assert_well_formed(image: &DecodedImage) {
    assert_eq!(image.rgba.len(), image.width as usize * image.height as usize * 4, "{image:?}");
    assert!(image.width > 0 && image.height > 0, "{image:?}");
}

/// True when the picture is more than one flat colour: a decoder that delivers nothing but
/// zeroes would pass every size check.
fn has_content(image: &DecodedImage) -> bool {
    let first = pixel(image, 0, 0);
    image.rgba.as_chunks::<4>().0.iter().any(|other| other.iter().zip(first.iter()).any(|(a, b)| a.abs_diff(*b) > 24))
}

/// The average red, green and blue of a picture: the same before and after scaling, and
/// different when a decoder mixes up the channels.
fn average_colour(image: &DecodedImage) -> [u64; 3] {
    let mut sums = [0_u64; 3];
    for pixel in image.rgba.as_chunks::<4>().0 {
        for channel in 0..3 {
            sums[channel] += u64::from(pixel[channel]);
        }
    }
    sums.map(|sum| sum / (u64::from(image.width) * u64::from(image.height)))
}

fn assert_same_average_colour(whole: &DecodedImage, small: &DecodedImage, name: &str) {
    let (large, little) = (average_colour(whole), average_colour(small));
    for channel in 0..3 {
        assert!(large[channel].abs_diff(little[channel]) <= 2, "{name}: {large:?} against {little:?}");
    }
}

// --- A picture whose four quarters have different colours -------------------------------------

const RED: [u8; 3] = [220, 40, 40];
const GREEN: [u8; 3] = [40, 200, 40];
const BLUE: [u8; 3] = [40, 40, 220];
const YELLOW: [u8; 3] = [230, 220, 40];
static QUARTER_COLOURS: [(&str, [u8; 3]); 4] = [("red", RED), ("green", GREEN), ("blue", BLUE), ("yellow", YELLOW)];

/// Top left red, top right green, bottom left blue, bottom right yellow.
fn quarters(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb(match (x < width / 2, y < height / 2) {
            (true, true) => RED,
            (false, true) => GREEN,
            (true, false) => BLUE,
            (false, false) => YELLOW,
        })
    })
}

fn colour_name(pixel: [u8; 4]) -> &'static str {
    let distance = |colour: &[u8; 3]| -> u32 {
        colour.iter().zip(pixel.iter()).map(|(a, b)| u32::from(a.abs_diff(*b)).pow(2)).sum()
    };
    let (name, colour) = QUARTER_COLOURS.iter().min_by_key(|(_, colour)| distance(colour)).expect("four colours");
    let name: &'static str = name;
    // Lossy formats shift the colours a little, never by this much.
    assert!(distance(colour) < 40 * 40 * 3, "{pixel:?} is none of the four colours");
    name
}

/// The colours in the middle of the four quarters: top left, top right, bottom left, bottom
/// right.
fn quarter_colours(image: &DecodedImage) -> [&'static str; 4] {
    let (left, right) = (image.width / 4, image.width * 3 / 4);
    let (top, bottom) = (image.height / 4, image.height * 3 / 4);
    [
        colour_name(pixel(image, left, top)),
        colour_name(pixel(image, right, top)),
        colour_name(pixel(image, left, bottom)),
        colour_name(pixel(image, right, bottom)),
    ]
}

/// What `quarters` looks like on screen for each EXIF orientation 1 to 8 (top left, top right,
/// bottom left, bottom right), written out by hand from the EXIF specification.
const QUARTERS_BY_EXIF_ORIENTATION: [[&str; 4]; 8] = [
    ["red", "green", "blue", "yellow"], // 1: as stored
    ["green", "red", "yellow", "blue"], // 2: mirrored left to right
    ["yellow", "blue", "green", "red"], // 3: turned half round
    ["blue", "yellow", "red", "green"], // 4: mirrored top to bottom
    ["red", "blue", "green", "yellow"], // 5: mirrored along the diagonal from the top left
    ["blue", "red", "yellow", "green"], // 6: a quarter turn clockwise
    ["yellow", "green", "blue", "red"], // 7: mirrored along the other diagonal
    ["green", "yellow", "red", "blue"], // 8: a quarter turn anticlockwise
];

/// A JPEG with an EXIF block that holds nothing but the orientation tag, put directly behind
/// the start marker as cameras do.
fn jpeg_with_orientation(picture: &RgbImage, orientation: u8) -> Vec<u8> {
    let jpeg = encode(&DynamicImage::ImageRgb8(picture.clone()), ImageFormat::Jpeg);
    assert_eq!(&jpeg[..2], [0xFF, 0xD8]);
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(b"MM\0\x2A\0\0\0\x08"); // big-endian TIFF header, first directory at 8
    exif.extend_from_slice(&[0, 1]); // one entry
    exif.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0]); // tag 274, SHORT, 1 value
    exif.extend_from_slice(&[0, 0, 0, 0]); // no further directory
    let length = u16::try_from(exif.len() + 2).expect("a short segment");

    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(&exif);
    bytes.extend_from_slice(&jpeg[2..]);
    bytes
}

/// An uncompressed little-endian RGB TIFF with an orientation tag: a format this app has no
/// decoder of its own for and every Windows has a codec for.
#[cfg(windows)]
fn tiff_with_orientation(picture: &RgbImage, orientation: u16) -> Vec<u8> {
    const ENTRIES: u16 = 10;
    let directory_end = 8 + 2 + u32::from(ENTRIES) * 12 + 4;
    let pixels_at = directory_end + 6;
    let short = |tag: u16, value: u16| -> Vec<u8> {
        [tag.to_le_bytes(), 3_u16.to_le_bytes(), 1_u16.to_le_bytes(), [0, 0], value.to_le_bytes(), [0, 0]].concat()
    };
    let long = |tag: u16, kind: u16, count: u32, value: u32| -> Vec<u8> {
        [&tag.to_le_bytes()[..], &kind.to_le_bytes()[..], &count.to_le_bytes()[..], &value.to_le_bytes()[..]].concat()
    };
    let (width, height) = picture.dimensions();
    let mut bytes = b"II\x2A\0\x08\0\0\0".to_vec();
    bytes.extend_from_slice(&ENTRIES.to_le_bytes());
    bytes.extend(short(256, width as u16)); // ImageWidth
    bytes.extend(short(257, height as u16)); // ImageLength
    bytes.extend(long(258, 3, 3, directory_end)); // BitsPerSample: three values behind the directory
    bytes.extend(short(259, 1)); // Compression: none
    bytes.extend(short(262, 2)); // PhotometricInterpretation: RGB
    bytes.extend(long(273, 4, 1, pixels_at)); // StripOffsets
    bytes.extend(short(274, orientation)); // Orientation
    bytes.extend(short(277, 3)); // SamplesPerPixel
    bytes.extend(short(278, height as u16)); // RowsPerStrip
    bytes.extend(long(279, 4, 1, width * height * 3)); // StripByteCounts
    bytes.extend_from_slice(&[0, 0, 0, 0]); // no further directory
    bytes.extend_from_slice(&[8, 0, 8, 0, 8, 0]);
    assert_eq!(bytes.len(), pixels_at as usize);
    bytes.extend_from_slice(picture.as_raw());
    bytes
}

// --- The fixtures -----------------------------------------------------------------------------

#[test]
fn decodes_the_fixture_pictures_whole() {
    for relative in [
        "loose/IMG_2.jpg",
        "loose/IMG_10.jpg",
        "loose/img_1.png",
        "loose/Sunset at the pier (final).webp",
        "export-basic/posts/post1.jpg",
        "export-basic/posts/post2.png",
        "export-basic/posts/post3.webp",
        "export-basic/avatars/fixture_tidewater.jpg",
    ] {
        let path = fixture(relative);
        let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|error| panic!("{relative}: {error}"));
        assert_well_formed(&image);
        assert!(has_content(&image), "{relative} is one flat colour");
        // Small enough for a texture: nothing is scaled.
        assert_eq!((image.width, image.height), (image.source_width, image.source_height), "{relative}");
        assert_eq!(probe_dimensions(&path).unwrap(), (image.width, image.height), "{relative}");
        // No transparency in these fixtures.
        assert!(image.rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255), "{relative}");
    }
    let post = decode_picture(&fixture("loose/IMG_2.jpg"), MAX_TEXTURE_SIDE).unwrap();
    assert_eq!((post.width, post.height), (720, 720));
}

#[test]
fn the_content_decides_the_decoder_not_the_extension() {
    // A WebP named .png and a PNG named .jpg (the clone's "lenient" fixture).
    for relative in ["export-lenient/posts/lenient2.png", "export-lenient/posts/lenient4.jpg"] {
        let path = fixture(relative);
        let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|error| panic!("{relative}: {error}"));
        assert_well_formed(&image);
        assert!(has_content(&image), "{relative}");
        assert_eq!(probe_dimensions(&path).unwrap(), (image.source_width, image.source_height));
    }
    // And the other way round: a JPEG without any extension, a JPEG named .heic.
    let folder = TempFolder::new("extension");
    let jpeg = fs::read(fixture("loose/IMG_2.jpg")).unwrap();
    for name in ["no extension", "really a jpeg.heic", "really a jpeg.txt"] {
        let image = decode_picture(&folder.write(name, &jpeg), 200).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!((image.width, image.height, image.source_width, image.source_height), (200, 200, 720, 720));
    }
}

#[test]
fn scales_the_fixtures_down_to_the_requested_side() {
    for relative in ["loose/IMG_2.jpg", "loose/img_1.png", "loose/Sunset at the pier (final).webp"] {
        let path = fixture(relative);
        let whole = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap();
        let small = decode_picture(&path, 180).unwrap();
        assert_well_formed(&small);
        assert_eq!((small.width, small.height), fit_within(whole.width, whole.height, 180), "{relative}");
        assert_eq!(small.width.max(small.height), 180, "{relative}");
        assert_eq!((small.source_width, small.source_height), (whole.width, whole.height), "{relative}");
        assert!(has_content(&small), "{relative}");
        // The small picture shows the same thing: its average colour is that of the whole one.
        assert_same_average_colour(&whole, &small, relative);

        let thumbnail = decode_thumbnail(&path, 100).unwrap();
        assert_well_formed(&thumbnail);
        assert_eq!((thumbnail.width, thumbnail.height), cover_within(whole.width, whole.height, 100), "{relative}");
        assert_eq!(thumbnail.width.min(thumbnail.height), 100, "{relative}");
        assert_eq!((thumbnail.source_width, thumbnail.source_height), (whole.width, whole.height));
        assert_same_average_colour(&whole, &thumbnail, relative);
    }
    // The three shapes of the fixtures: square, portrait and landscape.
    assert_eq!(probe_dimensions(&fixture("loose/IMG_2.jpg")).unwrap(), (720, 720));
    assert_eq!(probe_dimensions(&fixture("loose/img_1.png")).unwrap(), (720, 900));
    assert_eq!(probe_dimensions(&fixture("loose/Sunset at the pier (final).webp")).unwrap(), (960, 540));
    let portrait = decode_thumbnail(&fixture("loose/img_1.png"), 120).unwrap();
    assert_eq!((portrait.width, portrait.height), (120, 150));
    let landscape = decode_picture(&fixture("loose/Sunset at the pier (final).webp"), 640).unwrap();
    assert_eq!((landscape.width, landscape.height), (640, 360));
}

// --- HEIC through Windows Imaging Component ---------------------------------------------------

/// The HEIC fixture decoded by WIC, or `None` (with a note) when this computer cannot: the
/// HEIF Image Extensions are not installed, or this is not Windows.
fn heic_or_skip(test: &str) -> Option<DecodedImage> {
    match decode_with_wic(&fixture("loose/portrait.heic"), MAX_TEXTURE_SIDE) {
        Ok(image) => Some(image),
        Err(reason) if reason.contains(COMPONENT_NOT_FOUND) || reason == messages::DECODER_ONLY_ON_WINDOWS => {
            eprintln!("SKIPPED {test}: this computer has no HEIF codec ({reason})");
            None
        }
        Err(reason) => panic!("the HEIC fixture failed for another reason than a missing codec: {reason}"),
    }
}

#[test]
fn decodes_the_heic_fixture_through_windows_imaging_component() {
    let path = fixture("loose/portrait.heic");
    let Some(direct) = heic_or_skip("decodes_the_heic_fixture_through_windows_imaging_component") else {
        // Without the codec the sentence must name the remedy and the file.
        let error = decode_picture(&path, MAX_TEXTURE_SIDE).expect_err("no codec, no picture");
        assert_eq!(error.kind, DecodeErrorKind::NoDecoder);
        assert!(error.message.starts_with("\"portrait.heic\" could not be shown: "), "{error}");
        assert_eq!(probe_dimensions(&path).expect_err("no codec, no size").kind, DecodeErrorKind::NoDecoder);
        return;
    };
    assert_well_formed(&direct);
    assert_eq!((direct.width, direct.height, direct.source_width, direct.source_height), (720, 720, 720, 720));
    assert!(has_content(&direct));
    assert!(direct.rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));

    // decode_picture takes the same road for a format without a built-in decoder.
    assert_eq!(decode_picture(&path, MAX_TEXTURE_SIDE).unwrap(), direct);
    assert_eq!(probe_dimensions(&path).unwrap(), (720, 720));

    let small = decode_picture(&path, 90).unwrap();
    assert_well_formed(&small);
    assert_eq!((small.width, small.height, small.source_width, small.source_height), (90, 90, 720, 720));
    assert!(has_content(&small));
    assert!(small.rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255), "scaling must not invent transparency");
    // Red and blue in the right order, also on the scaled road.
    assert_same_average_colour(&direct, &small, "portrait.heic at 90");

    let thumbnail = decode_thumbnail(&path, DEFAULT_THUMBNAIL_SIDE).unwrap();
    assert_eq!((thumbnail.width, thumbnail.height), (480, 480));
    assert_same_average_colour(&direct, &thumbnail, "portrait.heic at 480");

    // The same file in the export fixture, and under a name with spaces and non-ASCII letters.
    assert_eq!(decode_picture(&fixture("export-basic/posts/post4.heic"), MAX_TEXTURE_SIDE).unwrap(), direct);
    let folder = TempFolder::new("heic-name");
    let renamed = folder.write("Été à la plage 🌅 (1).heic", &fs::read(&path).unwrap());
    assert_eq!(decode_picture(&renamed, MAX_TEXTURE_SIDE).unwrap(), direct);
}

#[test]
fn decodes_an_avif_picture_through_windows_imaging_component() {
    // fixtures/decode/icon.avif: 128 x 128, written by the AVIF encoder of the image crate.
    let path = fixture("decode/icon.avif");
    let whole = match decode_picture(&path, MAX_TEXTURE_SIDE) {
        Ok(image) => image,
        // AVIF needs the AV1 Video Extension on top of the HEIF Image Extensions; which error
        // Windows gives without it is not known, so every "no decoder" counts.
        Err(error) if error.kind == DecodeErrorKind::NoDecoder => {
            eprintln!("SKIPPED decodes_an_avif_picture_through_windows_imaging_component: {error}");
            assert!(error.message.starts_with("\"icon.avif\" could not be shown: "), "{error}");
            return;
        }
        Err(error) => panic!("the AVIF fixture failed for another reason than a missing codec: {error}"),
    };
    assert_well_formed(&whole);
    assert_eq!((whole.width, whole.height, whole.source_width, whole.source_height), (128, 128, 128, 128));
    assert!(has_content(&whole));
    assert_eq!(probe_dimensions(&path).unwrap(), (128, 128));

    let small = decode_picture(&path, 32).unwrap();
    assert_well_formed(&small);
    assert_eq!((small.width, small.height, small.source_width, small.source_height), (32, 32, 128, 128));
    eprintln!("AVIF centre: whole {:?}, scaled {:?}", pixel(&whole, 64, 64), pixel(&small, 16, 16));
    let (centre, scaled_centre) = (pixel(&whole, 64, 64), pixel(&small, 16, 16));
    for channel in 0..4 {
        assert!(centre[channel].abs_diff(scaled_centre[channel]) <= 40, "{centre:?} against {scaled_centre:?}");
    }
}

fn read_u32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

fn write_u32(bytes: &mut [u8], at: usize, value: usize) {
    bytes[at..at + 4].copy_from_slice(&u32::try_from(value).expect("a small file").to_be_bytes());
}

/// The offset and size of the first box of `kind` between `start` and `end`.
fn find_box(bytes: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> (usize, usize) {
    let mut at = start;
    while at + 8 <= end {
        let size = read_u32(bytes, at);
        if &bytes[at + 4..at + 8] == kind {
            return (at, size);
        }
        assert!(size >= 8, "a box without a size at {at}");
        at += size;
    }
    panic!("the fixture has no {:?} box", String::from_utf8_lossy(kind));
}

/// The HEIC fixture with one more property (such as a rotation) attached to its picture. The
/// fixture is simple enough to patch by hand: one item, properties in `iprp/ipco`, their
/// association in `iprp/ipma`, and the position of the coded picture in `iloc`, which moves
/// because the boxes in front of it grow.
fn heic_with_property(original: &[u8], property: &[u8]) -> Vec<u8> {
    let mut bytes = original.to_vec();
    let (meta, meta_size) = find_box(&bytes, 0, bytes.len(), b"meta");
    // A `meta` box has four bytes of version and flags before its children.
    let (iloc, _) = find_box(&bytes, meta + 12, meta + meta_size, b"iloc");
    let (iprp, iprp_size) = find_box(&bytes, meta + 12, meta + meta_size, b"iprp");
    let (ipco, ipco_size) = find_box(&bytes, iprp + 8, iprp + iprp_size, b"ipco");
    let (ipma, ipma_size) = find_box(&bytes, iprp + 8, iprp + iprp_size, b"ipma");
    assert!(ipma > ipco, "the fixture lists the properties before their associations");

    let mut property_count = 0;
    let mut at = ipco + 8;
    while at < ipco + ipco_size {
        at += read_u32(&bytes, at);
        property_count += 1;
    }

    // ipma: version and flags 0, one entry: item id (2 bytes), count (1 byte), one byte each.
    assert_eq!(read_u32(&bytes, ipma + 8), 0);
    assert_eq!(read_u32(&bytes, ipma + 12), 1);
    bytes[ipma + 18] += 1;
    // iloc: version 0, 4-byte offsets, lengths and base offsets, one item with its base offset
    // behind the item id and the data reference index.
    assert_eq!(bytes[iloc + 8..iloc + 16], [0, 0, 0, 0, 0x44, 0x40, 0, 1]);
    let grown = property.len() + 1;
    let base_offset = read_u32(&bytes, iloc + 20);
    write_u32(&mut bytes, iloc + 20, base_offset + grown);

    write_u32(&mut bytes, meta, meta_size + grown);
    write_u32(&mut bytes, iprp, iprp_size + grown);
    write_u32(&mut bytes, ipco, ipco_size + property.len());
    write_u32(&mut bytes, ipma, ipma_size + 1);
    // The association first: it lies behind the property list, so the offsets above stay valid.
    // 0x80 marks the property as essential, which a transformation has to be.
    bytes.insert(ipma + ipma_size, 0x80 | (property_count + 1) as u8);
    let ipco_end = ipco + ipco_size;
    bytes.splice(ipco_end..ipco_end, property.iter().copied());
    bytes
}

fn mean_difference(a: &RgbaImage, b: &RgbaImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions());
    let total: u64 = a.as_raw().iter().zip(b.as_raw().iter()).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f64 / a.as_raw().len() as f64
}

#[test]
fn a_rotated_heic_arrives_upright_from_its_decoder() {
    let Some(upright) = heic_or_skip("a_rotated_heic_arrives_upright_from_its_decoder") else {
        return;
    };
    let original = fs::read(fixture("loose/portrait.heic")).unwrap();
    // `irot` with angle 1: show the picture turned a quarter anticlockwise.
    let turned_file = heic_with_property(&original, &[0, 0, 0, 9, b'i', b'r', b'o', b't', 1]);
    let folder = TempFolder::new("heic-irot");
    let path = folder.write("turned.heic", &turned_file);
    let shown = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!((shown.width, shown.height), (720, 720));

    let upright = RgbaImage::from_raw(720, 720, upright.rgba).unwrap();
    let shown = RgbaImage::from_raw(720, 720, shown.rgba).unwrap();
    let candidates = [
        ("not turned", upright.clone()),
        ("a quarter clockwise", image::imageops::rotate90(&upright)),
        ("half round", image::imageops::rotate180(&upright)),
        ("a quarter anticlockwise", image::imageops::rotate270(&upright)),
    ];
    let differences: Vec<(&str, f64)> =
        candidates.iter().map(|(name, candidate)| (*name, mean_difference(candidate, &shown))).collect();
    eprintln!("HEIC with irot = 1 against the upright fixture: {differences:?}");
    let (closest, difference) =
        differences.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)).expect("four candidates");
    // The decoder of Windows applies the rotation of the file; this module must not add one.
    assert_eq!(closest, "a quarter anticlockwise", "{differences:?}");
    assert!(difference < 1.0, "{differences:?}");
}

// --- Orientation ------------------------------------------------------------------------------

#[test]
fn a_jpeg_is_turned_by_its_exif_orientation() {
    let folder = TempFolder::new("exif");
    let picture = quarters(60, 40);
    for orientation in 1..=8_u8 {
        let name = format!("orientation-{orientation}.jpg");
        let path = folder.write(&name, &jpeg_with_orientation(&picture, orientation));
        let expected_size = if orientation >= 5 { (40, 60) } else { (60, 40) };

        let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_well_formed(&image);
        assert_eq!((image.width, image.height), expected_size, "{name}");
        assert_eq!((image.source_width, image.source_height), expected_size, "{name}");
        assert_eq!(quarter_colours(&image), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");
        assert_eq!(probe_dimensions(&path).unwrap(), expected_size, "{name}");

        // Scaled: the limit applies to the picture as it is shown.
        let small = decode_picture(&path, 30).unwrap();
        let expected_small = if orientation >= 5 { (20, 30) } else { (30, 20) };
        assert_eq!((small.width, small.height), expected_small, "{name}");
        assert_eq!((small.source_width, small.source_height), expected_size, "{name}");
        assert_eq!(quarter_colours(&small), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");
    }
    // A JPEG without EXIF, and one with a value outside 1..=8, are shown as stored.
    let plain = folder.write("plain.jpg", &encode(&DynamicImage::ImageRgb8(picture.clone()), ImageFormat::Jpeg));
    assert_eq!(quarter_colours(&decode_picture(&plain, MAX_TEXTURE_SIDE).unwrap()), QUARTERS_BY_EXIF_ORIENTATION[0]);
    let odd = folder.write("odd.jpg", &jpeg_with_orientation(&picture, 9));
    let image = decode_picture(&odd, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!((image.width, image.height), (60, 40));
    assert_eq!(quarter_colours(&image), QUARTERS_BY_EXIF_ORIENTATION[0]);
}

#[cfg(windows)]
#[test]
fn windows_imaging_component_turns_a_jpeg_by_its_exif_orientation_too() {
    // The second chance for a JPEG the image crate cannot read must show it the same way.
    let folder = TempFolder::new("exif-wic");
    let picture = quarters(60, 40);
    for orientation in 1..=8_u8 {
        let name = format!("orientation-{orientation}.jpg");
        let path = folder.write(&name, &jpeg_with_orientation(&picture, orientation));
        let expected_size = if orientation >= 5 { (40, 60) } else { (60, 40) };
        let image = decode_with_wic(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        assert_well_formed(&image);
        assert_eq!((image.width, image.height), expected_size, "{name}");
        assert_eq!(quarter_colours(&image), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");

        let small = decode_with_wic(&path, 30).unwrap();
        assert_eq!((small.width, small.height), if orientation >= 5 { (20, 30) } else { (30, 20) }, "{name}");
        assert_eq!((small.source_width, small.source_height), expected_size, "{name}");
        assert_eq!(quarter_colours(&small), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");
    }
}

#[cfg(windows)]
#[test]
fn a_format_without_a_builtin_decoder_goes_through_windows_imaging_component() {
    // TIFF: unknown to the sniffer and to the image crate of this build, known to every Windows.
    let folder = TempFolder::new("tiff");
    let picture = quarters(48, 32);
    for orientation in [1_u16, 3, 6, 8] {
        let name = format!("scan-{orientation}.tif");
        let path = folder.write(&name, &tiff_with_orientation(&picture, orientation));
        let expected_size = if orientation >= 5 { (32, 48) } else { (48, 32) };
        let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_well_formed(&image);
        assert_eq!((image.width, image.height), expected_size, "{name}");
        assert_eq!(quarter_colours(&image), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");
        // Uncompressed and not scaled: the colours are exact.
        assert_eq!(pixel(&image, 0, 0)[3], 255);
        assert_eq!(probe_dimensions(&path).unwrap(), expected_size, "{name}");

        let thumbnail = decode_thumbnail(&path, 16).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), if orientation >= 5 { (16, 24) } else { (24, 16) }, "{name}");
        assert_eq!(quarter_colours(&thumbnail), QUARTERS_BY_EXIF_ORIENTATION[usize::from(orientation) - 1], "{name}");
    }
    let exact = decode_picture(&folder.write("exact.tif", &tiff_with_orientation(&picture, 1)), MAX_TEXTURE_SIDE).unwrap();
    assert_eq!(pixel(&exact, 0, 0), [RED[0], RED[1], RED[2], 255]);
    assert_eq!(pixel(&exact, 47, 31), [YELLOW[0], YELLOW[1], YELLOW[2], 255]);
}

// --- Size -------------------------------------------------------------------------------------

#[test]
fn a_camera_sized_picture_is_scaled_down_for_the_screen_and_the_grid() {
    let folder = TempFolder::new("large");
    let path = folder.write("camera.jpg", &encode(&DynamicImage::ImageRgb8(quarters(4000, 3000)), ImageFormat::Jpeg));
    assert_eq!(probe_dimensions(&path).unwrap(), (4000, 3000));

    let started = std::time::Instant::now();
    let screen = decode_picture(&path, 1000).unwrap();
    eprintln!("12 megapixel JPEG to 1000 x 750: {} ms", started.elapsed().as_millis());
    assert_well_formed(&screen);
    assert_eq!((screen.width, screen.height, screen.source_width, screen.source_height), (1000, 750, 4000, 3000));
    assert_eq!(quarter_colours(&screen), QUARTERS_BY_EXIF_ORIENTATION[0]);

    // A reduction of less than four: the Catmull-Rom filter alone.
    let half = decode_picture(&path, 2000).unwrap();
    assert_eq!((half.width, half.height), (2000, 1500));
    assert_eq!(quarter_colours(&half), QUARTERS_BY_EXIF_ORIENTATION[0]);

    let started = std::time::Instant::now();
    let thumbnail = decode_thumbnail(&path, DEFAULT_THUMBNAIL_SIDE).unwrap();
    eprintln!("12 megapixel JPEG to a 640 x 480 thumbnail: {} ms", started.elapsed().as_millis());
    assert_well_formed(&thumbnail);
    assert_eq!((thumbnail.width, thumbnail.height), (640, 480));
    assert_eq!((thumbnail.source_width, thumbnail.source_height), (4000, 3000));
    assert_eq!(quarter_colours(&thumbnail), QUARTERS_BY_EXIF_ORIENTATION[0]);
    // The border between two quarters stays where it was: sharp, not smeared over the tile.
    assert_eq!(colour_name(pixel(&thumbnail, 310, 120)), "red");
    assert_eq!(colour_name(pixel(&thumbnail, 330, 120)), "green");

    // Never scaled up.
    let whole = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!((whole.width, whole.height), (4000, 3000));
}

#[test]
fn a_picture_wider_than_a_texture_is_scaled_to_the_texture_limit() {
    let folder = TempFolder::new("wide");
    let strip = RgbImage::from_fn(9000, 60, |x, _| Rgb(if x < 4500 { RED } else { BLUE }));
    let path = folder.write("panorama.png", &encode(&DynamicImage::ImageRgb8(strip), ImageFormat::Png));
    let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap();
    assert_well_formed(&image);
    assert_eq!((image.width, image.height, image.source_width, image.source_height), (8192, 55, 9000, 60));
    assert_eq!(pixel(&image, 100, 27), [RED[0], RED[1], RED[2], 255]);
    assert_eq!(pixel(&image, 8000, 27), [BLUE[0], BLUE[1], BLUE[2], 255]);

    // A thumbnail limits the shorter side and still respects the texture limit.
    let thumbnail = decode_thumbnail(&path, 480).unwrap();
    assert_eq!((thumbnail.width, thumbnail.height), (8192, 55));
    let tiny = decode_thumbnail(&path, 30).unwrap();
    assert_eq!((tiny.width, tiny.height), (4500, 30));
}

/// The 54-byte header of a 24-bit BMP that claims `width` x `height` pixels, without any.
fn bmp_header(width: i32, height: i32) -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend_from_slice(&54_u32.to_le_bytes()); // file size (not checked)
    bytes.extend_from_slice(&[0; 4]); // reserved
    bytes.extend_from_slice(&54_u32.to_le_bytes()); // where the pixels start
    bytes.extend_from_slice(&40_u32.to_le_bytes()); // size of the info header
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // planes
    bytes.extend_from_slice(&24_u16.to_le_bytes()); // bits per pixel
    bytes.extend_from_slice(&[0; 24]); // no compression, sizes and palette left at 0
    bytes
}

#[test]
fn a_decompression_bomb_is_refused_before_any_pixel_is_decoded() {
    // 54 bytes that claim 30,000 x 20,000 pixels: 2.4 GB as RGBA.
    let folder = TempFolder::new("bomb");
    let path = folder.write("bomb.bmp", &bmp_header(30_000, 20_000));
    let started = std::time::Instant::now();
    for result in [decode_picture(&path, MAX_TEXTURE_SIDE), decode_picture(&path, 100), decode_thumbnail(&path, 100)] {
        let error = result.expect_err("far too large");
        assert_eq!(error.kind, DecodeErrorKind::TooLarge);
        assert_eq!(error.message, "\"bomb.bmp\" is too large to show (30,000 x 20,000 pixels).");
    }
    assert!(started.elapsed().as_secs() < 5, "the refusal must come from the header");
    // The layout can still learn the size.
    assert_eq!(probe_dimensions(&path).unwrap(), (30_000, 20_000));

    // Within the limit the same header is simply a picture without its pixels.
    let cut = folder.write("cut.bmp", &bmp_header(300, 200));
    let error = decode_picture(&cut, MAX_TEXTURE_SIDE).expect_err("no pixels in the file");
    assert_eq!(error.kind, DecodeErrorKind::Damaged, "{error}");
    assert!(error.message.starts_with("\"cut.bmp\" could not be shown: "), "{error}");
}

// --- Other formats and pixel layouts ----------------------------------------------------------

#[test]
fn an_animation_shows_its_first_frame() {
    let folder = TempFolder::new("gif");
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        let first = RgbaImage::from_pixel(40, 30, Rgba([RED[0], RED[1], RED[2], 255]));
        let second = RgbaImage::from_pixel(40, 30, Rgba([BLUE[0], BLUE[1], BLUE[2], 255]));
        encoder.encode_frames([Frame::new(first), Frame::new(second)]).unwrap();
    }
    let path = folder.write("animation.gif", &bytes);
    let image = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap();
    assert_well_formed(&image);
    assert_eq!((image.width, image.height), (40, 30));
    assert_eq!(colour_name(pixel(&image, 20, 15)), "red");
    assert_eq!(probe_dimensions(&path).unwrap(), (40, 30));
}

#[test]
fn bitmaps_grey_and_sixteen_bit_pictures_become_rgba() {
    let folder = TempFolder::new("layouts");
    let picture = quarters(64, 48);

    let bmp = folder.write("picture.bmp", &encode(&DynamicImage::ImageRgb8(picture.clone()), ImageFormat::Bmp));
    let image = decode_picture(&bmp, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!((image.width, image.height), (64, 48));
    assert_eq!(pixel(&image, 5, 5), [RED[0], RED[1], RED[2], 255]);
    assert_eq!(pixel(&image, 60, 40), [YELLOW[0], YELLOW[1], YELLOW[2], 255]);

    let grey = DynamicImage::ImageRgb8(picture.clone()).into_luma8();
    let expected = grey.get_pixel(5, 5)[0];
    let grey = folder.write("grey.png", &encode(&DynamicImage::ImageLuma8(grey), ImageFormat::Png));
    let image = decode_picture(&grey, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!(pixel(&image, 5, 5), [expected, expected, expected, 255]);
    let small = decode_picture(&grey, 32).unwrap();
    assert_eq!((small.width, small.height), (32, 24));
    assert_eq!(pixel(&small, 3, 3), [expected, expected, expected, 255]);

    let deep = DynamicImage::ImageRgb8(picture).into_rgb16();
    let deep = folder.write("deep.png", &encode(&DynamicImage::ImageRgb16(deep), ImageFormat::Png));
    let image = decode_picture(&deep, MAX_TEXTURE_SIDE).unwrap();
    assert_well_formed(&image);
    assert_eq!(pixel(&image, 5, 5), [RED[0], RED[1], RED[2], 255]);
    assert_eq!(quarter_colours(&decode_picture(&deep, 20).unwrap()), QUARTERS_BY_EXIF_ORIENTATION[0]);
}

#[test]
fn transparency_is_kept_straight_and_does_not_darken_scaled_edges() {
    let folder = TempFolder::new("alpha");
    // Left: fully transparent with a hidden black colour. Middle: half transparent orange.
    // Right: opaque white.
    let picture = RgbaImage::from_fn(120, 60, |x, _| match x {
        0..=36 => Rgba([0, 0, 0, 0]),
        37..=79 => Rgba([250, 140, 20, 128]),
        _ => Rgba([255, 255, 255, 255]),
    });
    let path = folder.write("logo.png", &encode(&DynamicImage::ImageRgba8(picture), ImageFormat::Png));

    // Not scaled: byte for byte what the file holds (straight alpha, not premultiplied).
    let whole = decode_picture(&path, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!(pixel(&whole, 10, 30), [0, 0, 0, 0]);
    assert_eq!(pixel(&whole, 50, 30), [250, 140, 20, 128]);
    assert_eq!(pixel(&whole, 100, 30), [255, 255, 255, 255]);

    let small = decode_picture(&path, 40).unwrap();
    assert_well_formed(&small);
    assert_eq!((small.width, small.height), (40, 20));
    assert_eq!(pixel(&small, 3, 10)[3], 0);
    assert_eq!(pixel(&small, 36, 10), [255, 255, 255, 255]);
    let middle = pixel(&small, 19, 10);
    assert!(middle[0].abs_diff(250) <= 2 && middle[1].abs_diff(140) <= 2 && middle[2].abs_diff(20) <= 2, "{middle:?}");
    assert_eq!(middle[3], 128);
    // Next to the transparent black, no visible pixel may be darker than the orange.
    for x in 0..40 {
        let [red, green, _, alpha] = pixel(&small, x, 10);
        // Below that the eight bits of a premultiplied colour are too coarse to judge.
        if alpha > 32 {
            assert!(red >= 225 && green >= 120, "pixel {x} is {:?}", pixel(&small, x, 10));
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_imaging_component_keeps_transparency_straight_as_well() {
    let folder = TempFolder::new("alpha-wic");
    let picture = RgbaImage::from_fn(120, 60, |x, _| match x {
        0..=36 => Rgba([0, 0, 0, 0]),
        37..=79 => Rgba([250, 140, 20, 128]),
        _ => Rgba([255, 255, 255, 255]),
    });
    let path = folder.write("logo.png", &encode(&DynamicImage::ImageRgba8(picture), ImageFormat::Png));

    let whole = decode_with_wic(&path, MAX_TEXTURE_SIDE).unwrap();
    assert_eq!(pixel(&whole, 10, 30)[3], 0);
    assert_eq!(pixel(&whole, 50, 30), [250, 140, 20, 128]);
    assert_eq!(pixel(&whole, 100, 30), [255, 255, 255, 255]);

    let small = decode_with_wic(&path, 40).unwrap();
    assert_well_formed(&small);
    assert_eq!((small.width, small.height, small.source_width, small.source_height), (40, 20, 120, 60));
    assert_eq!(pixel(&small, 3, 10)[3], 0);
    assert_eq!(pixel(&small, 36, 10), [255, 255, 255, 255]);
    let middle = pixel(&small, 19, 10);
    // Red, green, blue in this order, and divided by the alpha again.
    assert!(middle[0].abs_diff(250) <= 3 && middle[1].abs_diff(140) <= 3 && middle[2].abs_diff(20) <= 3, "{middle:?}");
    assert!(middle[3].abs_diff(128) <= 1, "{middle:?}");
    for x in 0..40 {
        let [red, green, _, alpha] = pixel(&small, x, 10);
        if alpha > 32 {
            assert!(red >= 225 && green >= 120, "pixel {x} is {:?}", pixel(&small, x, 10));
        }
    }
}

// --- Files that cannot be shown ---------------------------------------------------------------

#[test]
fn an_empty_file_is_reported_as_empty() {
    let path = fixture("loose/broken.jpg");
    for error in [
        decode_picture(&path, MAX_TEXTURE_SIDE).expect_err("nothing to decode"),
        decode_thumbnail(&path, 100).expect_err("nothing to decode"),
        probe_dimensions(&path).expect_err("nothing to measure"),
    ] {
        assert_eq!(error.kind, DecodeErrorKind::Damaged);
        assert_eq!(error.message, "\"broken.jpg\" is an empty file.");
    }
}

#[test]
fn a_missing_file_and_a_folder_are_unreadable() {
    let missing = fixture("loose/no-such-file.jpg");
    for error in [
        decode_picture(&missing, MAX_TEXTURE_SIDE).expect_err("no file"),
        probe_dimensions(&missing).expect_err("no file"),
    ] {
        assert_eq!(error.kind, DecodeErrorKind::Unreadable);
        assert!(error.message.starts_with("\"no-such-file.jpg\" could not be read: "), "{error}");
    }
    let folder = decode_picture(&fixture("loose"), MAX_TEXTURE_SIDE).expect_err("a folder is not a picture");
    assert_eq!(folder.kind, DecodeErrorKind::Unreadable);
    assert!(folder.message.starts_with("\"loose\" could not be read: "), "{folder}");
}

#[test]
fn a_file_that_is_no_picture_gets_a_sentence_with_its_name() {
    let path = fixture("loose/notes.txt");
    let error = decode_picture(&path, MAX_TEXTURE_SIDE).expect_err("text is not a picture");
    eprintln!("notes.txt: {error}");
    assert_eq!(error.kind, DecodeErrorKind::NoDecoder);
    if cfg!(windows) {
        // Windows has no decoder for text either, but no codec from the Store would help: the
        // sentence says that the file is not a supported picture, not what to install.
        assert_eq!(error.message, instagram_core::messages::file_not_supported("notes.txt"));
    } else {
        assert!(error.message.starts_with("\"notes.txt\" could not be shown: "), "{error}");
    }
    assert!(error.message.ends_with('.'), "{error}");
    assert_eq!(probe_dimensions(&path).expect_err("text has no size").kind, DecodeErrorKind::NoDecoder);
}

#[test]
fn a_video_is_not_decoded() {
    let folder = TempFolder::new("video");
    let mut bytes = b"\0\0\0\x20ftypisom\0\0\0\0isomiso2avc1mp41".to_vec();
    bytes.resize(4096, 0);
    let path = folder.write("clip.mp4", &bytes);
    for error in [
        decode_picture(&path, MAX_TEXTURE_SIDE).expect_err("a video"),
        probe_dimensions(&path).expect_err("a video"),
    ] {
        assert_eq!(error.kind, DecodeErrorKind::Damaged);
        assert_eq!(error.message, "\"clip.mp4\" is a video, not a picture.");
    }
}

#[test]
fn truncated_pictures_fail_with_a_sentence_or_show_what_is_there() {
    let folder = TempFolder::new("truncated");
    for (relative, name) in [
        ("loose/IMG_2.jpg", "cut.jpg"),
        ("loose/img_1.png", "cut.png"),
        ("loose/Sunset at the pier (final).webp", "cut.webp"),
        ("loose/portrait.heic", "cut.heic"),
    ] {
        let whole = fs::read(fixture(relative)).unwrap();
        // The size the header promises; unknown when this computer cannot read HEIC.
        let promised = probe_dimensions(&fixture(relative)).ok();
        for (label, keep) in [("half", whole.len() / 2), ("head", 300), ("magic", 16)] {
            let path = folder.write(name, &whole[..keep]);
            match decode_picture(&path, MAX_TEXTURE_SIDE) {
                // A decoder may show the part that is there, as browsers do.
                Ok(image) => {
                    eprintln!("{name} ({label}): shown as {image:?}");
                    assert_well_formed(&image);
                    assert_eq!(Some((image.width, image.height)), promised, "{name} ({label})");
                    // A substitute in one flat colour shows nothing: that must be an error instead.
                    let (pixels, _) = image.rgba.as_chunks::<4>();
                    assert!(pixels.iter().any(|pixel| pixel != &pixels[0]), "{name} ({label}) is shown as an empty picture");
                }
                Err(error) => {
                    eprintln!("{name} ({label}): {error}");
                    let expected: &[DecodeErrorKind] = if name == "cut.heic" {
                        // Damaged with the codec, "no decoder" without it.
                        &[DecodeErrorKind::Damaged, DecodeErrorKind::NoDecoder]
                    } else {
                        &[DecodeErrorKind::Damaged]
                    };
                    assert!(expected.contains(&error.kind), "{name} ({label}): {error:?}");
                    assert!(error.message.starts_with(&format!("\"{name}\" could not be shown: ")), "{error}");
                    assert!(error.message.ends_with('.'), "{error}");
                }
            }
            // Measuring must not panic either; whether a header survives depends on the format.
            let _ = probe_dimensions(&path);
            let _ = decode_thumbnail(&path, 100);
        }
    }
}

#[test]
fn garbage_behind_a_valid_signature_never_panics() {
    let folder = TempFolder::new("garbage");
    let signatures: [(&str, &[u8]); 6] = [
        ("noise.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]),
        ("noise.png", &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        ("noise.gif", b"GIF89a"),
        ("noise.webp", b"RIFF\x40\0\0\0WEBPVP8 "),
        ("noise.bmp", b"BM\x40\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0"),
        ("noise.heic", b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
    ];
    for (name, signature) in signatures {
        // A fixed pseudo-random tail, so that a failure can be repeated.
        let mut bytes = signature.to_vec();
        let mut state = 0x2545_F491_u32;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            bytes.push((state >> 24) as u8);
        }
        let path = folder.write(name, &bytes);
        for result in [decode_picture(&path, 256).map(|_| ()), probe_dimensions(&path).map(|_| ())] {
            if let Err(error) = result {
                assert!(error.message.contains(name), "{error}");
                assert!(!error.message.is_empty());
            }
        }
    }
}

// --- Threads and files ------------------------------------------------------------------------

#[test]
fn several_worker_threads_decode_at_once() {
    let heic = heic_or_skip("several_worker_threads_decode_at_once (HEIC part)").is_some();
    let mut names = vec!["loose/IMG_2.jpg", "loose/img_1.png", "loose/Sunset at the pier (final).webp"];
    if heic {
        names.push("loose/portrait.heic");
    }
    let workers: Vec<_> = (0..6)
        .map(|worker| {
            let names = names.clone();
            std::thread::spawn(move || {
                for round in 0..3 {
                    let relative = names[(worker + round) % names.len()];
                    let image = decode_thumbnail(&fixture(relative), 120)
                        .unwrap_or_else(|error| panic!("worker {worker}, {relative}: {error}"));
                    assert_well_formed(&image);
                    assert_eq!(image.width.min(image.height), 120, "{relative}");
                    let size = probe_dimensions(&fixture(relative)).unwrap();
                    assert_eq!(size, (image.source_width, image.source_height), "{relative}");
                    assert_eq!((image.width, image.height), cover_within(size.0, size.1, 120), "{relative}");
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("a worker panicked");
    }
}

#[cfg(windows)]
#[test]
fn windows_imaging_component_also_works_on_a_thread_with_its_own_apartment() {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    // A thread that already lives in a single-threaded apartment, as the UI thread does: the
    // decoder must use COM as it finds it and leave it initialised.
    let worker = std::thread::spawn(|| {
        // SAFETY: initialises COM for this thread; balanced by the CoUninitialize below.
        let entered = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        assert!(entered, "a fresh thread can enter a single-threaded apartment");
        let first = decode_with_wic(&fixture("loose/IMG_2.jpg"), 100);
        let second = decode_with_wic(&fixture("loose/IMG_10.jpg"), 100);
        // SAFETY: balances the successful CoInitializeEx above on the same thread.
        unsafe { CoUninitialize() };
        (first, second)
    });
    let (first, second) = worker.join().expect("the worker panicked");
    for image in [first.expect("first decode"), second.expect("second decode")] {
        assert_eq!((image.width, image.height, image.source_width, image.source_height), (100, 100, 720, 720));
    }
}

#[test]
fn a_decoded_file_is_closed_again() {
    // The user may rename or delete a picture while the app shows it.
    let folder = TempFolder::new("closed");
    let mut names = vec![("loose/IMG_2.jpg", "a.jpg"), ("loose/img_1.png", "a.png")];
    if heic_or_skip("a_decoded_file_is_closed_again (HEIC part)").is_some() {
        names.push(("loose/portrait.heic", "a.heic"));
    }
    for (relative, name) in names {
        let path = folder.write(name, &fs::read(fixture(relative)).unwrap());
        decode_picture(&path, 64).unwrap_or_else(|error| panic!("{name}: {error}"));
        probe_dimensions(&path).unwrap();
        let renamed = folder.path.join(format!("renamed-{name}"));
        fs::rename(&path, &renamed).unwrap_or_else(|error| panic!("{name} is still open: {error}"));
        fs::remove_file(&renamed).unwrap_or_else(|error| panic!("{name} is still open: {error}"));
    }
    // Through WIC as well, for a format every Windows decodes.
    if cfg!(windows) {
        let path = folder.write("wic.jpg", &fs::read(fixture("loose/IMG_2.jpg")).unwrap());
        decode_with_wic(&path, 64).unwrap();
        fs::remove_file(&path).expect("the file is closed after decoding through WIC");
    }
}
