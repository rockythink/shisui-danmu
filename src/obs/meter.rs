pub const SILENCE_DB: f32 = -60.0;
const RELEASE_DB_PER_SECOND: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicrophoneLevel {
    pub peak_db: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LevelSmoother {
    displayed_db: f32,
}

impl Default for LevelSmoother {
    fn default() -> Self {
        Self {
            displayed_db: SILENCE_DB,
        }
    }
}

impl LevelSmoother {
    pub(crate) fn update(&mut self, sample_db: f32, elapsed_seconds: f32) -> MicrophoneLevel {
        let sample_db = sample_db.clamp(SILENCE_DB, 0.0);
        if sample_db >= self.displayed_db {
            self.displayed_db = sample_db;
        } else {
            self.displayed_db = (self.displayed_db
                - RELEASE_DB_PER_SECOND * elapsed_seconds.max(0.0))
            .max(sample_db)
            .max(SILENCE_DB);
        }
        MicrophoneLevel {
            peak_db: self.displayed_db,
        }
    }
}

pub(crate) fn peak_db(levels: &[[f32; 3]]) -> f32 {
    let peak = levels
        .iter()
        .map(|level| level[1])
        .filter(|level| level.is_finite())
        .fold(0.0_f32, f32::max);
    multiplier_to_db(peak)
}

fn multiplier_to_db(multiplier: f32) -> f32 {
    if multiplier <= 0.0 || !multiplier.is_finite() {
        SILENCE_DB
    } else {
        (20.0 * multiplier.log10()).clamp(SILENCE_DB, 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_obs_multiplier_to_dbfs() {
        assert_eq!(multiplier_to_db(1.0), 0.0);
        assert!((multiplier_to_db(0.1) + 20.0).abs() < 0.001);
        assert_eq!(multiplier_to_db(0.0), SILENCE_DB);
    }

    #[test]
    fn selects_the_loudest_channel_peak() {
        let levels = [[0.01, 0.1, 0.2], [0.02, 0.5, 0.7]];
        assert!((peak_db(&levels) + 6.0206).abs() < 0.001);
    }

    #[test]
    fn attacks_immediately_and_releases_gradually() {
        let mut smoother = LevelSmoother::default();
        assert_eq!(smoother.update(-6.0, 0.1).peak_db, -6.0);
        assert!((smoother.update(-60.0, 0.1).peak_db + 7.8).abs() < 0.001);
    }
}
