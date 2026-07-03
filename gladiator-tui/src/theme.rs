use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub error: String,
    pub warning: String,
    pub success: String,
    pub info: String,
    pub text: String,
    pub text_muted: String,
    pub background: String,
    pub background_panel: String,
    pub background_element: String,
    pub border: String,
    pub border_active: String,
    pub border_subtle: String,
}

impl Theme {
    /// opencode dark theme
    pub fn default_dark() -> Self {
        Self {
            primary: "#fab283".to_string(),
            secondary: "#5c9cf5".to_string(),
            accent: "#9d7cd8".to_string(),
            error: "#e06c75".to_string(),
            warning: "#f5a742".to_string(),
            success: "#7fd88f".to_string(),
            info: "#56b6c2".to_string(),
            text: "#eeeeee".to_string(),
            text_muted: "#808080".to_string(),
            background: "#0a0a0a".to_string(),
            background_panel: "#141414".to_string(),
            background_element: "#1e1e1e".to_string(),
            border: "#484848".to_string(),
            border_active: "#606060".to_string(),
            border_subtle: "#3c3c3c".to_string(),
        }
    }

    pub fn to_color(&self, hex: &str) -> Color {
        if let Some(c) = parse_hex_color(hex) {
            c
        } else {
            Color::Reset
        }
    }

    pub fn color_primary(&self) -> Color {
        self.to_color(&self.primary)
    }
    pub fn color_secondary(&self) -> Color {
        self.to_color(&self.secondary)
    }
    pub fn color_accent(&self) -> Color {
        self.to_color(&self.accent)
    }
    pub fn color_error(&self) -> Color {
        self.to_color(&self.error)
    }
    pub fn color_warning(&self) -> Color {
        self.to_color(&self.warning)
    }
    pub fn color_success(&self) -> Color {
        self.to_color(&self.success)
    }
    pub fn color_info(&self) -> Color {
        self.to_color(&self.info)
    }
    pub fn color_text(&self) -> Color {
        self.to_color(&self.text)
    }
    pub fn color_text_muted(&self) -> Color {
        self.to_color(&self.text_muted)
    }
    pub fn color_background(&self) -> Color {
        self.to_color(&self.background)
    }
    pub fn color_background_panel(&self) -> Color {
        self.to_color(&self.background_panel)
    }
    pub fn color_background_element(&self) -> Color {
        self.to_color(&self.background_element)
    }
    pub fn color_border(&self) -> Color {
        self.to_color(&self.border)
    }
    pub fn color_border_active(&self) -> Color {
        self.to_color(&self.border_active)
    }
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}
