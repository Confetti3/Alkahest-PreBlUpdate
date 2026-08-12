use std::num::NonZeroU16;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub vsync: bool,
    pub resolution_scale: f32,
    pub framerate_limit: Option<NonZeroU16>,
    #[serde(rename = "framelimiter_enabled", skip_serializing)]
    legacy_framelimiter_enabled: Option<bool>,
}

impl AppConfig {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn normalize(&mut self) {
        if self.legacy_framelimiter_enabled == Some(false) {
            self.framerate_limit = None;
        }
        self.legacy_framelimiter_enabled = None;
        self.schema_version = Self::SCHEMA_VERSION;
        if !self.resolution_scale.is_finite() {
            self.resolution_scale = 1.0;
        }
        self.resolution_scale = self.resolution_scale.clamp(0.25, 2.0);
        self.framerate_limit = self
            .framerate_limit
            .filter(|limit| (20..=1000).contains(&limit.get()));
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            vsync: true,
            resolution_scale: 1.0,
            framerate_limit: None,
            legacy_framelimiter_enabled: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_disabled_limiter_migrates_to_uncapped() {
        let mut config: AppConfig = toml::from_str(
            "vsync = true\nresolution_scale = 1.0\nframerate_limit = 60\nframelimiter_enabled = \
             false",
        )
        .unwrap();
        config.normalize();
        assert_eq!(config.framerate_limit, None);
    }

    #[test]
    fn invalid_values_are_rejected_without_frame_arithmetic() {
        assert!(toml::from_str::<AppConfig>("framerate_limit = 0").is_err());

        let mut config = AppConfig {
            resolution_scale: f32::NAN,
            framerate_limit: NonZeroU16::new(u16::MAX),
            ..Default::default()
        };
        config.normalize();
        assert_eq!(config.resolution_scale, 1.0);
        assert_eq!(config.framerate_limit, None);
    }
}
