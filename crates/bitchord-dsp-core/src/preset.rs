use crate::eq::EqLayout;

/// Built-in 7-band presets tailored for [`EqLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EqualizerPreset {
    Flat,
    Acoustic,
    BassBoost,
    BassCut,
    Vocal,
    TrebleBoost,
    TrebleCut,
    Loudness,
    SpokenWord,
    Electronic,
    Rock,
    HipHop,
    Jazz,
    Classical,
    SmallSpeakers,
    LateNight,
    Custom,
}

impl EqualizerPreset {
    pub const ALL: [Self; 17] = [
        Self::Flat,
        Self::Acoustic,
        Self::BassBoost,
        Self::BassCut,
        Self::Vocal,
        Self::TrebleBoost,
        Self::TrebleCut,
        Self::Loudness,
        Self::SpokenWord,
        Self::Electronic,
        Self::Rock,
        Self::HipHop,
        Self::Jazz,
        Self::Classical,
        Self::SmallSpeakers,
        Self::LateNight,
        Self::Custom,
    ];

    pub const fn bands_db(self) -> &'static [f32] {
        match self {
            Self::Flat => &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            Self::Acoustic => &[3.0, 1.5, 0.0, 1.5, 2.5, 2.0, 1.0],
            Self::BassBoost => &[6.0, 4.0, 1.5, 0.0, 0.0, 0.0, 0.0],
            Self::BassCut => &[-6.0, -4.0, -1.5, 0.0, 0.0, 0.0, 0.0],
            Self::Vocal => &[-3.0, -1.5, 1.0, 3.5, 3.0, 1.0, -1.0],
            Self::TrebleBoost => &[0.0, 0.0, 0.0, 0.0, 1.5, 3.5, 5.0],
            Self::TrebleCut => &[0.0, 0.0, 0.0, 0.0, -1.5, -3.5, -5.0],
            Self::Loudness => &[6.0, 3.5, 0.0, -1.5, -1.0, 2.0, 5.0],
            Self::SpokenWord => &[-5.0, -2.5, 1.5, 4.0, 3.5, 1.5, -2.0],
            Self::Electronic => &[5.0, 3.0, -1.0, 0.0, 1.0, 3.0, 4.0],
            Self::Rock => &[4.0, 2.5, -1.0, -1.5, 1.0, 3.0, 3.5],
            Self::HipHop => &[6.0, 4.0, 0.5, -1.0, 0.5, 2.0, 2.5],
            Self::Jazz => &[3.0, 1.5, 0.0, 1.0, 1.5, 2.0, 2.5],
            Self::Classical => &[3.0, 2.0, 0.0, 0.0, 1.0, 2.5, 3.0],
            Self::SmallSpeakers => &[5.0, 4.0, 2.0, 0.5, 0.0, -1.0, -2.0],
            Self::LateNight => &[3.0, 1.0, 0.0, 1.5, 1.0, -1.0, -3.0],
            Self::Custom => &[],
        }
    }

    /// Returns the preset matching `bands_db` (within 0.05 dB per band), or [`Self::Custom`].
    pub fn matching(bands_db: &[f32]) -> Self {
        if bands_db.len() != EqLayout::MANUAL_COUNT {
            return Self::Custom;
        }
        for preset in Self::ALL {
            if preset == Self::Custom {
                continue;
            }
            let target = preset.bands_db();
            if target
                .iter()
                .zip(bands_db.iter())
                .all(|(a, b)| (a - b).abs() < 0.05)
            {
                return preset;
            }
        }
        Self::Custom
    }
}
