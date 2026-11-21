/// Channel-linked, zero-lookahead, instant-attack peak catcher sitting under
/// the equalizer's partial make-up attenuation.
#[derive(Debug, Clone)]
pub struct PeakLimiter {
    release_coef: f32,
    reduction: f32,
}

impl Default for PeakLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakLimiter {
    /// Ceiling just under full scale so float and 16-bit PCM round cleanly without clipping.
    pub const CEILING: f32 = 0.985;

    const RELEASE_SECONDS: f64 = 0.1;

    pub const fn new() -> Self {
        Self {
            release_coef: 1.0,
            reduction: 1.0,
        }
    }

    pub fn configure(&mut self, sample_rate: u32) {
        self.release_coef = if sample_rate == 0 {
            1.0
        } else {
            (1.0 - (-1.0 / (Self::RELEASE_SECONDS * f64::from(sample_rate))).exp()) as f32
        };
        self.reduction = 1.0;
    }

    pub fn reset(&mut self) {
        self.reduction = 1.0;
    }

    /// Computes gain multiplier for a frame given the peak absolute sample across its channels.
    pub fn gain_for(&mut self, peak: f32) -> f32 {
        let wanted = if peak > Self::CEILING {
            Self::CEILING / peak
        } else {
            1.0
        };
        self.reduction = if wanted < self.reduction {
            wanted
        } else {
            self.reduction + (wanted - self.reduction) * self.release_coef
        };
        self.reduction
    }
}
