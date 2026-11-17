//! The command line: paths to open, the auto view options, the browser pages' options, and the
//! hidden harness options.
//!
//! Explorer passes the files of "Open with" (and of a drag onto the executable) as plain
//! arguments, so everything that is not an option is a path. The auto view options are parsed
//! by the core crate (`--autoview[=SECONDS]`, `--loop=0|1`, `--shuffle=0|1`, `--wait=0|1`); the
//! options in
//! `harness.rs` are for development and automated screenshots. `shortcuts.rs` lists all of them
//! for `--help`, which `main.rs` answers together with `--version` before anything is parsed.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use instagram_core::autoview::{self, AutoViewRequest};
use instagram_core::settings::{EnvLookup, ProcessEnv};
use instagram_core::store::Theme;
use instagram_core::video_rules::VideoCaps;

use crate::harness::{self, HarnessError, HarnessOptions};
use crate::shortcuts::{OPTIONS, OPTION_HELP, OPTION_VERSION};
use crate::strings;
use crate::web::WebOverrides;

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommandLine {
    /// Pictures and folders to open at the start.
    pub paths: Vec<PathBuf>,
    /// `--autoview` and its companions, for the run only.
    pub auto_view: Option<AutoViewRequest>,
    /// The hidden development options.
    pub harness: HarnessOptions,
    /// `--no-web`, `--host`, `--frontend-port` and `--backend-port`, for this run.
    pub web: WebOverrides,
}

/// A port of the command line: a whole number from 1 to 65535.
fn parse_port(option: &str, given: &str) -> Result<u16, HarnessError> {
    match given.trim().parse::<u32>() {
        Ok(port @ 1..=65535) if given.trim().chars().all(|character| character.is_ascii_digit()) => Ok(port as u16),
        _ => Err(HarnessError { message: strings::cli_bad_port(option, given) }),
    }
}

/// Parses the arguments after the program name.
pub fn parse(arguments: &[OsString]) -> Result<CommandLine, HarnessError> {
    let parsed = autoview::parse_arguments(arguments);
    let mut command_line = CommandLine { auto_view: parsed.auto_view, ..CommandLine::default() };
    let mut rest = parsed.rest.into_iter();
    while let Some(argument) = rest.next() {
        let text = argument.to_string_lossy().into_owned();
        // The value of an option is the next argument, unless that is an option itself: a
        // forgotten value must not turn the next option into a folder (`--profile-dir` is
        // what keeps a run away from the real profile).
        let mut value = |name: &str| {
            rest.next()
                .filter(|next| !next.to_string_lossy().starts_with("--"))
                .ok_or_else(|| HarnessError { message: format!("{name} needs a value.") })
        };
        match text.as_str() {
            "--profile-dir" => command_line.harness.profile_dir = Some(PathBuf::from(value("--profile-dir")?)),
            "--screenshot" => command_line.harness.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--size" => command_line.harness.size = Some(harness::parse_size(&value("--size")?.to_string_lossy())?),
            "--scale" => {
                let given = value("--scale")?.to_string_lossy().into_owned();
                match given.parse::<f32>() {
                    Ok(scale) if (0.25..=8.0).contains(&scale) => command_line.harness.scale = Some(scale),
                    _ => return Err(HarnessError { message: format!("--scale expects pixels per point between 0.25 and 8, not \"{given}\".") }),
                }
            }
            "--theme" => {
                let given = value("--theme")?.to_string_lossy().into_owned();
                command_line.harness.theme = Some(match given.as_str() {
                    "light" => Theme::Light,
                    "dark" => Theme::Dark,
                    _ => return Err(HarnessError { message: format!("--theme expects light or dark, not \"{given}\".") }),
                });
            }
            "--scenario" => {
                let given = value("--scenario")?.to_string_lossy().into_owned();
                command_line.harness.scenario.extend(harness::parse_scenario(&given)?);
            }
            "--watchdog" => {
                let given = value("--watchdog")?.to_string_lossy().into_owned();
                match given.parse::<u64>() {
                    Ok(seconds) if seconds > 0 => command_line.harness.watchdog_seconds = Some(seconds),
                    _ => return Err(HarnessError { message: format!("--watchdog expects a number of seconds, not \"{given}\".") }),
                }
            }
            "--no-web" => command_line.web.no_web = true,
            "--host" => {
                let given = value("--host")?.to_string_lossy().into_owned();
                command_line.web.host = Some(given.trim().parse::<Ipv4Addr>().map_err(|_| HarnessError { message: strings::cli_bad_host(&given) })?);
            }
            "--frontend-port" | "--frontend" => {
                let given = value(&text)?.to_string_lossy().into_owned();
                command_line.web.frontend_port = Some(parse_port(&text, &given)?);
            }
            "--backend-port" | "--backend" => {
                let given = value(&text)?.to_string_lossy().into_owned();
                command_line.web.backend_port = Some(parse_port(&text, &given)?);
            }
            // Hidden (video brief 5.3): the video rules of a graphics card or of a software
            // renderer, whatever the adapter, for screenshots and tests. Not in --help.
            "--video-adapter" => {
                let given = value("--video-adapter")?.to_string_lossy().into_owned();
                command_line.harness.video_adapter = Some(VideoCaps::parse(&given).ok_or_else(|| HarnessError { message: format!("--video-adapter expects cpu or gpu, not \"{given}\".") })?);
            }
            "--frame-log" => command_line.harness.frame_log = true,
            "--animate" => command_line.harness.animate = true,
            // Answered by `shortcuts::information` before the command line is parsed.
            OPTION_HELP | OPTION_VERSION => {}
            other if other.starts_with("--") => {
                let options: Vec<&str> = OPTIONS.iter().map(|option| option.syntax).collect();
                return Err(HarnessError { message: format!("Unknown option \"{other}\". Options: {}; everything else is a picture or a folder to open.", options.join(", ")) });
            }
            _ => command_line.paths.push(PathBuf::from(argument)),
        }
    }
    if let (Some(frontend), Some(backend)) = (command_line.web.frontend_port, command_line.web.backend_port) {
        if frontend == backend {
            return Err(HarnessError { message: strings::CLI_SAME_PORTS.to_owned() });
        }
    }
    Ok(command_line)
}

/// The environment of a run with `--profile-dir`: `APPDATA` is the given folder, `LOCALAPPDATA`
/// its subfolder `Local` and `TEMP` / `TMP` its subfolder `Temp`; `INSTAGRAM_CACHE_DIR` is not
/// set; everything else is the real environment. With it the settings file, the data folder and
/// the archive cache live under that folder, the web app's real library is not found, and the
/// user's real profile is never touched.
pub struct ProfileEnv {
    /// The folder that stands in for %APPDATA%.
    pub profile_dir: PathBuf,
}

/// The subfolder of the profile folder that stands in for %LOCALAPPDATA%.
pub const PROFILE_LOCAL_FOLDER: &str = "Local";
/// The subfolder of the profile folder that stands in for %TEMP% and %TMP%.
pub const PROFILE_TEMP_FOLDER: &str = "Temp";

impl EnvLookup for ProfileEnv {
    fn var(&self, name: &str) -> Option<String> {
        let is = |variable: &str| name.eq_ignore_ascii_case(variable);
        if is("APPDATA") {
            Some(self.profile_dir.display().to_string())
        } else if is("LOCALAPPDATA") {
            Some(self.profile_dir.join(PROFILE_LOCAL_FOLDER).display().to_string())
        } else if is("TEMP") || is("TMP") {
            Some(self.profile_dir.join(PROFILE_TEMP_FOLDER).display().to_string())
        } else if is(instagram_core::settings::WEB_APP_LIBRARY_VARIABLE) {
            None
        } else {
            ProcessEnv.var(name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Step;

    fn arguments(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn plain_arguments_are_paths_in_their_order() {
        let command_line = parse(&arguments(&["C:\\Pictures\\a.jpg", "D:\\folder with spaces"])).expect("parse");
        assert_eq!(command_line.paths, vec![PathBuf::from("C:\\Pictures\\a.jpg"), PathBuf::from("D:\\folder with spaces")]);
        assert_eq!(command_line.auto_view, None);
        assert!(!command_line.harness.is_active());
    }

    #[test]
    fn auto_view_options_are_taken_out_by_the_core() {
        let command_line = parse(&arguments(&["--autoview=30", "--shuffle=1", "pictures"])).expect("parse");
        let request = command_line.auto_view.expect("auto view");
        assert_eq!(request.interval_sec, Some(30));
        assert_eq!(request.shuffle, Some(true));
        assert_eq!(command_line.paths, vec![PathBuf::from("pictures")]);
    }

    #[test]
    fn harness_options_are_read_with_their_values() {
        let command_line =
            parse(&arguments(&["--profile-dir", "p", "--screenshot", "s.png", "--size", "1100x800", "--scale", "1.0", "--theme", "dark", "--scenario", "feed,post:4", "--scenario", "more-menu", "fixture"]))
                .expect("parse");
        let harness = &command_line.harness;
        assert_eq!(harness.profile_dir, Some(PathBuf::from("p")));
        assert_eq!(harness.screenshot, Some(PathBuf::from("s.png")));
        assert_eq!(harness.size, Some((1100.0, 800.0)));
        assert_eq!(harness.scale, Some(1.0));
        assert_eq!(harness.theme, Some(Theme::Dark));
        assert_eq!(harness.scenario, vec![Step::Feed, Step::Post(4), Step::MoreMenu]);
        assert_eq!(command_line.paths, vec![PathBuf::from("fixture")]);
    }

    #[test]
    fn mistakes_are_reported_and_never_opened_as_paths() {
        assert!(parse(&arguments(&["--screenshot"])).expect_err("value").message.contains("--screenshot needs a value"));
        // A forgotten value does not swallow the next option.
        let swallowed = parse(&arguments(&["--profile-dir", "--screenshot", "s.png", "--watchdog", "60"])).expect_err("value");
        assert_eq!(swallowed.message, "--profile-dir needs a value.");
        assert!(parse(&arguments(&["--theme", "blue"])).is_err());
        assert!(parse(&arguments(&["--scale", "0"])).is_err());
        assert!(parse(&arguments(&["--scenario", "teleport:1"])).expect_err("unknown step").message.contains("Valid steps"));
        assert_eq!(parse(&arguments(&["--video-adapter", "gpu"])).expect("adapter").harness.video_adapter, Some(VideoCaps::Gpu));
        assert!(parse(&arguments(&["--video-adapter", "warp"])).is_err());
        let unknown = parse(&arguments(&["--frobnicate"])).expect_err("unknown option").message;
        assert!(unknown.starts_with("Unknown option \"--frobnicate\". Options: --autoview[=SECONDS], --loop=0|1, --shuffle=0|1, --wait=0|1, --help, --version, --no-web, --host ADDRESS, --frontend-port PORT, --backend-port PORT;"), "{unknown}");
    }

    #[test]
    fn the_browser_options_are_read_with_their_values() {
        let command_line = parse(&arguments(&["--no-web", "--host", "0.0.0.0", "--frontend-port", "4000", "--backend", "4001", "pictures"])).expect("parse");
        assert_eq!(command_line.web, WebOverrides { no_web: true, host: Some(Ipv4Addr::UNSPECIFIED), frontend_port: Some(4000), backend_port: Some(4001) });
        assert_eq!(command_line.paths, vec![PathBuf::from("pictures")]);
        assert_eq!(parse(&arguments(&["--frontend", "3127"])).expect("alias").web.frontend_port, Some(3127));
        assert_eq!(parse(&arguments(&["--backend-port", "8317"])).expect("long form").web.backend_port, Some(8317));
        assert_eq!(parse(&arguments(&["--frontend-port", "0"])).expect_err("zero").message, "--frontend-port expects a port number from 1 to 65535, not \"0\".");
        assert_eq!(parse(&arguments(&["--backend", "70000"])).expect_err("too big").message, "--backend expects a port number from 1 to 65535, not \"70000\".");
        assert_eq!(parse(&arguments(&["--host", "localhost"])).expect_err("a name").message, "--host expects an IPv4 address such as 127.0.0.1 or 0.0.0.0, not \"localhost\".");
        assert_eq!(parse(&arguments(&["--frontend-port", "5000", "--backend-port", "5000"])).expect_err("the same port").message, "The browser pages and the API need two different ports.");
        assert_eq!(parse(&arguments(&["--host"])).expect_err("no value").message, "--host needs a value.");
    }

    #[test]
    fn the_information_options_are_not_paths() {
        let command_line = parse(&arguments(&["--help", "--version", "a.jpg"])).expect("parse");
        assert_eq!(command_line.paths, vec![PathBuf::from("a.jpg")]);
    }

    #[test]
    fn the_profile_folder_stands_in_for_the_folders_of_the_profile() {
        let env = ProfileEnv { profile_dir: PathBuf::from("X:\\profile") };
        assert_eq!(env.var("APPDATA").as_deref(), Some("X:\\profile"));
        assert_eq!(env.var("appdata").as_deref(), Some("X:\\profile"));
        assert_eq!(env.var("LOCALAPPDATA").as_deref(), Some("X:\\profile\\Local"));
        assert_eq!(env.var("TEMP").as_deref(), Some("X:\\profile\\Temp"));
        assert_eq!(env.var("tmp").as_deref(), Some("X:\\profile\\Temp"));
        assert_eq!(env.var("INSTAGRAM_CACHE_DIR"), None, "the web app's real library is never found");
        assert_eq!(env.var("USERPROFILE"), ProcessEnv.var("USERPROFILE"));
        // So the archive cache and the web app's library lie in the profile folder too.
        let local = instagram_core::settings::local_dir(&env, std::path::Path::new("X:\\profile\\data"));
        assert_eq!(local, PathBuf::from("X:\\profile\\Local\\instagram-windows-app"));
        assert_eq!(instagram_core::settings::web_app_library_dir(&env), Some(PathBuf::from("X:\\profile\\Temp\\instagram_cache")));
    }
}
