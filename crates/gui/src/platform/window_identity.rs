//! Linux desktops match a window to its launcher by this ID. X11 also accepts
//! icon pixels directly; Wayland obtains the icon from the installed launcher.

pub const APP_ID: &str = "io.github.NexusOnePlus.Wasp";

pub fn apply(options: &mut gpui::WindowOptions, cx: &gpui::App) {
    options.app_id = Some(APP_ID.to_owned());
    imp::apply(options, cx);
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn apply(options: &mut gpui::WindowOptions, cx: &gpui::App) {
        match icon(cx) {
            Ok(icon) => options.icon = Some(std::sync::Arc::new(icon)),
            Err(error) => log::warn!("could not load the window icon: {error}"),
        }
    }

    fn icon(_: &gpui::App) -> anyhow::Result<image::RgbaImage> {
        Ok(image::load_from_memory(include_bytes!(
            "../../../../packaging/linux/io.github.NexusOnePlus.Wasp.png"
        ))?
        .to_rgba8())
    }
}

#[cfg(windows)]
mod imp {
    pub(super) fn apply(options: &mut gpui::WindowOptions, _: &gpui::App) {
        let bytes = include_bytes!("../../../../packaging/windows/wasp.png");
        match image::load_from_memory(bytes) {
            Ok(icon) => options.icon = Some(std::sync::Arc::new(icon.to_rgba8())),
            Err(error) => log::warn!("could not load the window icon: {error}"),
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub(super) fn apply(_: &mut gpui::WindowOptions, _: &gpui::App) {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[gpui::test]
    fn linux_window_has_the_launcher_id_and_straight_rgba_icon(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut options = gpui::WindowOptions::default();
            super::apply(&mut options, cx);
            assert_eq!(options.app_id.as_deref(), Some(super::APP_ID));
            let icon = options.icon.expect("the Wasp PNG produces a window icon");
            assert_eq!(icon.dimensions(), (256, 256));
            // The lime tile is the mark's background.
            assert_eq!(icon.get_pixel(20, 128).0, [208, 253, 55, 255]);
        });
    }
}
