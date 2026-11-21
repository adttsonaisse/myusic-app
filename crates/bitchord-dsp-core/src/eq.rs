const MAKEUP_FRACTION: f32 = 0.5;
const RESPONSE_POINTS: usize = 96;

/// Shape of one filter section in [`EqLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterKind {
    Bell,
    LowShelf,
    HighShelf,
}

/// Fixed identity of one EQ slot: filter shape and corner/center frequency in Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSlot {
    pub kind: FilterKind,
    pub frequency_hz: f32,
}

/// Fixed 10-slot filter layout shared by the 7-band manual EQ (slots 0..=6)
/// and the tone pad (slots 7..=9).
pub struct EqLayout;

impl EqLayout {
    pub const MANUAL_BANDS_HZ: [f32; 7] = [60.0, 150.0, 400.0, 1_000.0, 2_500.0, 6_000.0, 14_000.0];
    pub const MANUAL_Q: f32 = 1.0;
    pub const MANUAL_FIRST: usize = 0;
    pub const MANUAL_COUNT: usize = 7;

    pub const TONE_LOW: usize = 7;
    pub const TONE_MID: usize = 8;
    pub const TONE_HIGH: usize = 9;

    pub const SLOTS: usize = 10;

    pub const MANUAL_RANGE_DB: f32 = 12.0;
    pub const TONE_STEPS: i32 = 5;
    pub const TONE_DB_PER_STEP: f32 = 1.2;
    pub const TONE_TILT_HZ: [f32; 2] = [250.0, 4_000.0];

    pub const SLOTS_SPEC: [FilterSlot; Self::SLOTS] = [
        FilterSlot {
            kind: FilterKind::LowShelf,
            frequency_hz: 60.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 150.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 400.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 1_000.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 2_500.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 6_000.0,
        },
        FilterSlot {
            kind: FilterKind::HighShelf,
            frequency_hz: 14_000.0,
        },
        FilterSlot {
            kind: FilterKind::LowShelf,
            frequency_hz: 250.0,
        },
        FilterSlot {
            kind: FilterKind::Bell,
            frequency_hz: 1_000.0,
        },
        FilterSlot {
            kind: FilterKind::HighShelf,
            frequency_hz: 4_000.0,
        },
    ];
}

/// Rendered tuning across all 10 [`EqLayout`] slots, with computed make-up attenuation.
#[derive(Debug, Clone, PartialEq)]
pub struct EqCurve {
    pub gains_db: [f32; EqLayout::SLOTS],
    pub qs: [f32; EqLayout::SLOTS],
    /// Make-up attenuation (`<= 0.0` dB): half of the curve's peak summed boost.
    pub preamp_db: f32,
}

impl EqCurve {
    pub fn flat() -> Self {
        Self::of([0.0; EqLayout::SLOTS], [0.707; EqLayout::SLOTS])
    }

    pub fn of(gains_db: [f32; EqLayout::SLOTS], qs: [f32; EqLayout::SLOTS]) -> Self {
        let preamp_db = preamp_for(&gains_db, &qs);
        Self {
            gains_db,
            qs,
            preamp_db,
        }
    }
}

/// Renders the 7 manual sliders into a 10-slot curve; tone pad slots stay flat.
pub fn manual_curve(bands_db: &[f32]) -> EqCurve {
    let mut gains = [0.0; EqLayout::SLOTS];
    let mut qs = [0.707; EqLayout::SLOTS];
    for band in 0..EqLayout::MANUAL_COUNT {
        let db = bands_db.get(band).copied().unwrap_or(0.0);
        gains[EqLayout::MANUAL_FIRST + band] =
            db.clamp(-EqLayout::MANUAL_RANGE_DB, EqLayout::MANUAL_RANGE_DB);
        qs[EqLayout::MANUAL_FIRST + band] = EqLayout::MANUAL_Q;
    }
    EqCurve::of(gains, qs)
}

/// Renders the 2D tone pad (`x`: warm↔bright tilt, `y`: mid contour, `focused`: narrower Q).
pub fn tone_curve(x: i32, y: i32, focused: bool) -> EqCurve {
    let mut gains = [0.0; EqLayout::SLOTS];
    let mut qs = [0.707; EqLayout::SLOTS];
    let steps = EqLayout::TONE_STEPS;
    let tilt = (x.clamp(-steps, steps) as f32) * EqLayout::TONE_DB_PER_STEP;
    let contour = (y.clamp(-steps, steps) as f32) * EqLayout::TONE_DB_PER_STEP;

    gains[EqLayout::TONE_LOW] = -tilt;
    gains[EqLayout::TONE_HIGH] = tilt;
    gains[EqLayout::TONE_MID] = contour;

    let shelf_q = if focused { 0.9 } else { 0.5 };
    let bell_q = if focused { 2.2 } else { 0.7 };
    qs[EqLayout::TONE_LOW] = shelf_q;
    qs[EqLayout::TONE_HIGH] = shelf_q;
    qs[EqLayout::TONE_MID] = bell_q;

    EqCurve::of(gains, qs)
}

fn preamp_for(gains_db: &[f32; EqLayout::SLOTS], qs: &[f32; EqLayout::SLOTS]) -> f32 {
    let mut peak = 0.0_f32;
    for point in 0..RESPONSE_POINTS {
        let hz = response_frequency(point);
        let mut sum = 0.0_f32;
        for slot in 0..EqLayout::SLOTS {
            sum += section_gain_db(EqLayout::SLOTS_SPEC[slot], gains_db[slot], qs[slot], hz);
        }
        if sum > peak {
            peak = sum;
        }
    }
    -peak * MAKEUP_FRACTION
}

fn response_frequency(point: usize) -> f32 {
    let fraction = (point as f64) / ((RESPONSE_POINTS - 1) as f64);
    (20.0 * 1_000.0_f64.powf(fraction)) as f32
}

/// Magnitude response (in dB) of one analog prototype section at `hz`.
pub fn section_gain_db(slot: FilterSlot, gain_db: f32, q: f32, hz: f32) -> f32 {
    if gain_db.abs() < 0.01 {
        return 0.0;
    }
    let a = 10.0_f64.powf(f64::from(gain_db) / 40.0);
    let a2 = a * a;
    let x = f64::from(hz) / f64::from(slot.frequency_hz);
    let x2 = x * x;
    let qq = f64::from(q) * f64::from(q);

    let magnitude = match slot.kind {
        FilterKind::Bell => {
            let flat = (1.0 - x2) * (1.0 - x2);
            ((flat + x2 * a2 / qq) / (flat + x2 / (a2 * qq))).sqrt()
        }
        FilterKind::LowShelf => {
            let common = x2 * a / qq;
            a * (((a - x2) * (a - x2) + common) / ((1.0 - a * x2) * (1.0 - a * x2) + common)).sqrt()
        }
        FilterKind::HighShelf => {
            let common = x2 * a / qq;
            a * (((1.0 - a * x2) * (1.0 - a * x2) + common) / ((a - x2) * (a - x2) + common)).sqrt()
        }
    };
    (20.0 * magnitude.log10()) as f32
}
