use serde::{Deserialize, Serialize};

/// A YouTube Music watch queue to start via the `next` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchTarget {
    pub playlist_id: String,
    #[serde(default)]
    pub video_id: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
}

/// The cards of Home's "Top Picks for You" row, in layout order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopPickKind {
    LatestRelease,
    PersonalStation,
    Mood,
    ArtistAndSimilar,
    FeaturedArtist,
    NewReleaseMix,
    DiscoveryStation,
}

const CHILL_CATEGORY: &str = "ggMPOg1uX1JOQWZFeDByc2Jm";
const COMMUTE: &str = "ggMPOg1uX044Z2o5WERLckpU";
const ENERGIZE: &str = "ggMPOg1uX2lRZUZiMnNrQnJW";
const FEEL_GOOD_CATEGORY: &str = "ggMPOg1uXzZQbDB5eThLRTQ3";
const FOCUS_CATEGORY: &str = "ggMPOg1uX0NvNGNhWThMYWRh";
const GAMING_CATEGORY: &str = "ggMPOg1uX3NmUVV4Vzl3WGQ0";
const PARTY_CATEGORY: &str = "ggMPOg1uX0pmQ0s2V0JRclZs";
const ROMANCE: &str = "ggMPOg1uX0FzQ2FhZWtUY211";
const SAD: &str = "ggMPOg1uX0JLQ0gySWZKZVY1";
const SLEEP_CATEGORY: &str = "ggMPOg1uX1MxaFQ3Z0JMZkN4";
const WORKOUT: &str = "ggMPOg1uX09LWkhnTjRGRUJh";

/// Mood stations and activities backed by YouTube Music's 11 mood categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopPickMood {
    // Find Your Mood (core moods: 0..=6)
    Energy,
    Relax,
    FeelingBlue,
    FeelGood,
    Focus,
    Heartbreak,
    Love,

    // Moods and Activities
    AfterHours,
    AloneTime,
    Chill,
    Commuting,
    EatingCooking,
    Evening,
    Fitness,
    Gaming,
    Home,
    Morning,
    Motivation,
    Outdoors,
    Party,
    Sleep,
    Social,
    Sports,
    Summertime,
    Holiday,
    Weekend,
    Wellbeing,
    Work,
}

impl TopPickMood {
    pub const CATEGORY_BROWSE_ID: &'static str = "FEmusic_moods_and_genres_category";

    /// Language-independent YouTube Music category params id.
    pub const fn params(self) -> &'static str {
        match self {
            Self::Energy | Self::Motivation | Self::Sports => ENERGIZE,
            Self::Relax
            | Self::AfterHours
            | Self::AloneTime
            | Self::Chill
            | Self::EatingCooking
            | Self::Evening
            | Self::Home
            | Self::Wellbeing => CHILL_CATEGORY,
            Self::FeelingBlue | Self::Heartbreak => SAD,
            Self::FeelGood | Self::Morning | Self::Summertime | Self::Holiday => FEEL_GOOD_CATEGORY,
            Self::Focus | Self::Work => FOCUS_CATEGORY,
            Self::Love => ROMANCE,
            Self::Commuting => COMMUTE,
            Self::Fitness | Self::Outdoors => WORKOUT,
            Self::Gaming => GAMING_CATEGORY,
            Self::Party | Self::Social | Self::Weekend => PARTY_CATEGORY,
            Self::Sleep => SLEEP_CATEGORY,
        }
    }

    /// Shelf title keywords steering a mood to the right shelves within its category.
    pub const fn shelf_words(self) -> &'static [&'static str] {
        match self {
            Self::Relax => &["unwind", "peaceful", "relaxing", "laidback"],
            Self::FeelingBlue => &["sad", "melanchol", "lonely"],
            Self::Heartbreak => &["heartbreak", "breakup", "break-up", "broken", "heartache"],
            Self::AfterHours => &["deep grooves", "electronic", "jazz", "r&b"],
            Self::AloneTime => &["mellow", "acoustic", "laidback"],
            Self::Chill => &["chill", "chilled"],
            Self::EatingCooking => &["coffee", "soul", "jazz", "r&b"],
            Self::Evening => &["unwind", "acoustic", "laidback", "jazz"],
            Self::Fitness => &["gym", "cardio", "essentials"],
            Self::Home => &["coffee shop", "chill pop", "laidback"],
            Self::Morning => &["happy", "confident", "pop"],
            Self::Motivation => &["motivation", "power", "beast"],
            Self::Outdoors => &["outdoor", "running"],
            Self::Social => &["party today", "chill party", "throwback"],
            Self::Sports => &["beast", "power", "hip-hop"],
            Self::Summertime => &["dance", "kicking back", "feel-good pop"],
            Self::Holiday => &["throwback", "happy"],
            Self::Weekend => &["party today", "dance"],
            Self::Wellbeing => &["mindfulness", "meditation", "yoga"],
            Self::Work => &["flow", "instrumental", "reading"],
            Self::Energy
            | Self::FeelGood
            | Self::Focus
            | Self::Love
            | Self::Commuting
            | Self::Gaming
            | Self::Party
            | Self::Sleep => &[],
        }
    }

    /// One of the seven core "Find Your Mood" stations rather than an activity.
    pub const fn is_core_mood(self) -> bool {
        matches!(
            self,
            Self::Energy
                | Self::Relax
                | Self::FeelingBlue
                | Self::FeelGood
                | Self::Focus
                | Self::Heartbreak
                | Self::Love
        )
    }
}

/// One card of the Top Picks row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopPick {
    pub kind: TopPickKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub artist_name: Option<String>,
    #[serde(default)]
    pub artist_browse_id: Option<String>,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub browse_id: Option<String>,
    #[serde(default)]
    pub watch: Option<WatchTarget>,
    #[serde(default)]
    pub mood: Option<TopPickMood>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub hints: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_moods_match_upstream_boundary() {
        assert!(TopPickMood::Energy.is_core_mood());
        assert!(TopPickMood::Love.is_core_mood());
        assert!(!TopPickMood::AfterHours.is_core_mood());
        assert!(!TopPickMood::Work.is_core_mood());
        assert_eq!(
            TopPickMood::Heartbreak.params(),
            TopPickMood::FeelingBlue.params()
        );
        assert!(TopPickMood::Wellbeing.shelf_words().contains(&"yoga"));
    }
}
