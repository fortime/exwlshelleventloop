use std::{borrow::Cow, fs::File};

use iced::{Font, Pixels};

use crate::reexport::{Anchor, KeyboardInteractivity, Layer};

use layershellev::reexport::wayland_client::wl_keyboard::KeymapFormat;

#[derive(Debug)]
pub struct VirtualKeyboardSettings {
    pub file: File,
    pub keymap_size: u32,
    pub keymap_format: KeymapFormat,
}

#[derive(Debug)]
pub struct Settings<Flags> {
    /// The identifier of the application.
    ///
    /// If provided, this identifier may be used to identify the application or
    /// communicate with it through the windowing system.
    pub id: Option<String>,

    /// settings for layer shell
    pub layer_settings: LayerShellSettings,
    /// The data needed to initialize an [`Application`].
    ///
    /// [`Application`]: crate::Application
    pub flags: Flags,

    /// The fonts to load on boot.
    pub fonts: Vec<Cow<'static, [u8]>>,

    /// The default [`Font`] to be used.
    ///
    /// By default, it uses [`Family::SansSerif`](iced::font::Family::SansSerif).
    pub default_font: Font,

    /// The text size that will be used by default.
    ///
    /// The default value is `16.0`.
    pub default_text_size: Pixels,

    /// If set to true, the renderer will try to perform antialiasing for some
    /// primitives.
    ///
    /// Enabling it can produce a smoother result in some widgets, like the
    /// `Canvas`, at a performance cost.
    ///
    /// By default, it is disabled.
    ///
    pub antialiasing: bool,

    pub virtual_keyboard_support: Option<VirtualKeyboardSettings>,
}

impl<Flags> Default for Settings<Flags>
where
    Flags: Default,
{
    fn default() -> Self {
        Settings {
            id: None,
            flags: Default::default(),
            fonts: Vec::new(),
            layer_settings: LayerShellSettings::default(),
            default_font: Font::default(),
            default_text_size: Pixels(16.0),
            antialiasing: false,
            virtual_keyboard_support: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayerShellSettings {
    pub anchor: Anchor,
    pub layer: Layer,
    pub exclusive_zone: i32,
    pub size: Option<(u32, u32)>,
    pub margin: (i32, i32, i32, i32),
    pub keyboard_interactivity: KeyboardInteractivity,
    pub binded_output_name: Option<String>,
}

impl Default for LayerShellSettings {
    fn default() -> Self {
        LayerShellSettings {
            anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
            layer: Layer::Top,
            exclusive_zone: -1,
            size: None,
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            binded_output_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_default() {
        let settings: Settings<()> = Settings::default();

        assert!(settings.id.is_none());
        assert!(settings.fonts.is_empty());
        assert_eq!(settings.default_font, Font::default());
        assert_eq!(settings.default_text_size, Pixels(16.0));
        assert_eq!(settings.antialiasing, false);
        assert!(settings.virtual_keyboard_support.is_none());

        // Test default layershellv settings
        assert_eq!(settings.layer_settings.anchor, Anchor::Bottom | Anchor::Left | Anchor::Right);
        assert_eq!(settings.layer_settings.layer, Layer::Top);
        assert_eq!(settings.layer_settings.exclusive_zone, -1);
        assert_eq!(settings.layer_settings.size, None);
        assert_eq!(settings.layer_settings.margin, (0, 0, 0, 0));
        assert_eq!(settings.layer_settings.keyboard_interactivity, KeyboardInteractivity::OnDemand);
        assert!(settings.layer_settings.binded_output_name.is_none());
    }

    #[test]
    fn test_virtual_keyboard_settings() {
        let file = File::open("/dev/null").expect("Failed to open file");
        let keymap_size = 1024;
        let keymap_format = KeymapFormat::XkbV1;

        let virtual_keyboard_settings = VirtualKeyboardSettings {
            file,
            keymap_size,
            keymap_format,
        };

        assert_eq!(virtual_keyboard_settings.keymap_size, 1024);
        assert_eq!(virtual_keyboard_settings.keymap_format, KeymapFormat::XkbV1);
    }

    #[test]
    fn test_layer_shell_settings_custom() {
        let layer_settings = LayerShellSettings {
            anchor: Anchor::Top | Anchor::Left,
            layer: Layer::Background,
            exclusive_zone: 0,
            size: Some((1920, 1080)),
            margin: (10, 10, 10, 10),
            keyboard_interactivity: KeyboardInteractivity::None,
            binded_output_name: Some("HDMI-1".to_string()),
        };

        assert_eq!(layer_settings.anchor, Anchor::Top | Anchor::Left);
        assert_eq!(layer_settings.layer, Layer::Background);
        assert_eq!(layer_settings.exclusive_zone, 0);
        assert_eq!(layer_settings.size, Some((1920, 1080)));
        assert_eq!(layer_settings.margin, (10, 10, 10, 10));
        assert_eq!(layer_settings.keyboard_interactivity, KeyboardInteractivity::None);
        assert_eq!(layer_settings.binded_output_name, Some("HDMI-1".to_string()));
    }
}

