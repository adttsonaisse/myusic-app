pub mod eq;
pub mod limiter;
pub mod preset;
pub mod processor;
pub mod transition_filter;

pub use eq::{
    EqCurve, EqLayout, FilterKind, FilterSlot, manual_curve, section_gain_db, tone_curve,
};
pub use limiter::PeakLimiter;
pub use preset::EqualizerPreset;
pub use processor::{EqualizerProcessor, clamp16_from_float};
pub use transition_filter::TransitionFilter;
