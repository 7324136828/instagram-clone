# Vendored crates

`egui-wgpu/` is the crate `egui-wgpu` 0.37.0 exactly as published on crates.io (MIT OR
Apache-2.0, by the egui authors), with one change. The workspace `Cargo.toml` puts it in place of
the published crate (`[patch.crates-io]`), so every crate that uses egui-wgpu, eframe among them,
gets this copy.

## The change

On a machine without a usable graphics adapter, Direct3D 12 draws with the Microsoft Basic Render
Driver (WARP) on the processor. There, egui's fragment shader is slow because it contains a
hardware texture sample (`textureSample`): a viewer frame of 1920 x 1290 pixels took about
690 ms of processor time with it, 190 ms with the same shader filtering by `textureLoad` (four
taps), and 25 ms with no texture access at all. It does not help to take the branch without the
sample at run time (egui's `predictable_texture_filtering`): a shader that merely contains the
sample stays slow.

So when `RendererOptions::predictable_texture_filtering` is set, the render pipeline now uses two
new fragment entry points, `fs_main_gamma_framebuffer_in_shader` and
`fs_main_linear_framebuffer_in_shader`, which filter exactly as the predictable branch of egui's
`sample_texture` does (nearest or bilinear with four taps at texel centres, the same wrap
modes) and contain no `textureSample`. One shortcut gives the same colours with less work:
egui draws every flat-coloured shape with the texture coordinate (0, 0), where all taps of a
clamped texture are texel (0, 0), so a single load is made there (a third less processor time
again for a viewer frame). Without the option nothing changes.

The app sets the option only when the adapter is a processor (`wgpu::DeviceType::Cpu`), see
`app/src/rendering.rs`; a machine with a graphics card keeps the hardware sampler.

Changed files, each change marked "instagram-windows-app patch":

- `src/egui.wgsl`: the two entry points and their helper functions, appended at the end.
- `src/renderer.rs`: the choice of the fragment entry point in `Renderer::new`, and a sentence in
  the documentation of `predictable_texture_filtering`.

`egui-wgpu.patch` beside this file is that change alone, as a unified diff against the published
crate. To check that the folder is the published crate plus the change and nothing else, copy the
published files (`Cargo.toml`, `Cargo.toml.orig`, `README.md`, `src/`) to an empty folder, run
`git -c core.autocrlf=false apply <path>\egui-wgpu.patch` there (no repository is needed), and
compare the result with `egui-wgpu/`.

## Upgrading egui

When egui is upgraded, check whether the new egui-wgpu still compiles `textureSample` into the
shader of its predictable path. If it does, copy the new published crate here (the files of
`~/.cargo/registry/src/*/egui-wgpu-<version>/`: `Cargo.toml`, `Cargo.toml.orig`, `README.md`,
`src/`) and apply the change again; if it does not, remove this folder and the
`[patch.crates-io]` entry.
