use std::collections::HashMap;

use quick_xml::XmlVersion;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::alignments::line_alignments;
use crate::gaps::with_instrumental_gaps;
use crate::line::{LyricLine, LyricSyllable, LyricWord};

/// Roles that are alternate renderings of the same words and would double the line up.
const SKIPPED_ROLES: [&str; 2] = ["x-translation", "x-roman"];

/// The answering vocal (`ttm:role="x-bg"`), collected apart as [`LyricLine::background`].
const BACKGROUND_ROLE: &str = "x-bg";

/// Hard limit on XML element nesting depth to guard against malicious inputs.
const MAX_XML_DEPTH: usize = 256;

/// Apple Music's word-timed TTML lyric format parser.
///
/// A document contains `<p>` per sung line, each holding one `<span>` per
/// syllable with its own `begin`/`end`. Adjacent spans with no whitespace
/// between them belong to one word (`"e"` + `"nough"` -> `"enough"`), keeping
/// each span's timing as a [`LyricSyllable`].
pub struct TtmlLyrics;

impl TtmlLyrics {
    /// Parses a TTML XML document into [`LyricLine`]s with duet alignments and
    /// instrumental gaps. Malformed XML or documents with `<!DOCTYPE>` yield an
    /// empty `Vec` rather than panicking or failing loudly.
    pub fn parse(ttml: &str) -> Vec<LyricLine> {
        let Ok(root) = parse_xml(ttml) else {
            return Vec::new();
        };

        let mut paragraphs = Vec::new();
        collect_elements_by_name(&root, "p", &mut paragraphs);

        let mut sung: Vec<(LyricLine, Option<String>)> = Vec::with_capacity(paragraphs.len());
        for paragraph in paragraphs {
            let Some(line) = line_from(paragraph) else {
                continue;
            };
            let agent = paragraph.qualified("ttm:agent");
            let singer = (!agent.is_empty()).then(|| agent.to_owned());
            sung.push((line, singer));
        }

        sung.sort_by_key(|(line, _)| line.time_ms);

        let singers: Vec<Option<&str>> = sung.iter().map(|(_, singer)| singer.as_deref()).collect();
        let types = agent_types(&root);
        let sides = line_alignments(&singers, &types);

        let aligned: Vec<LyricLine> = sung
            .into_iter()
            .zip(sides)
            .map(|((mut line, _), side)| {
                line.alignment = side;
                line
            })
            .collect();

        with_instrumental_gaps(aligned)
    }

    /// Parses TTML clock values (`27.395`, `1:05.20`, `1:02:03.4`, `1.5s`,
    /// `250ms`) into milliseconds.
    pub fn time(value: Option<&str>) -> Option<i64> {
        let raw = value?.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(ms_part) = raw.strip_suffix("ms") {
            let ms = ms_part.parse::<f64>().ok()?;
            return ms.is_finite().then_some(ms as i64);
        }
        let stripped = raw.strip_suffix('s').unwrap_or(raw);
        let mut parts = stripped.split(':');
        let first = parts.next()?.parse::<f64>().ok()?;
        let seconds = match (parts.next(), parts.next(), parts.next()) {
            (None, None, None) => first,
            (Some(s), None, None) => first * 60.0 + s.parse::<f64>().ok()?,
            (Some(m), Some(s), None) => {
                first * 3_600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?
            }
            _ => return None,
        };
        if !seconds.is_finite() {
            return None;
        }
        Some((seconds * 1_000.0) as i64)
    }
}

#[derive(Debug)]
struct XmlElement {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<XmlNode>,
}

#[derive(Debug)]
enum XmlNode {
    Element(XmlElement),
    Text(String),
}

impl XmlElement {
    fn from_start(start: &BytesStart<'_>) -> Result<Self, ()> {
        let name = start.name().into_inner().to_owned();
        let mut attributes = Vec::new();
        for attr in start.attributes() {
            let attr = attr.map_err(|_| ())?;
            let key = attr.key.into_inner().to_owned();
            let val = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|_| ())?
                .into_owned();
            attributes.push((key, val));
        }
        Ok(Self {
            name,
            attributes,
            children: Vec::new(),
        })
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(XmlNode::Text(existing)) = self.children.last_mut() {
            existing.push_str(text);
        } else {
            self.children.push(XmlNode::Text(text.to_owned()));
        }
    }

    fn attr(&self, name: &str) -> &str {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// Reads a prefix-qualified attribute (`ttm:agent`, `ttm:role`, `xml:id`)
    /// whether the document wrote the full qualified name, bare local name, or
    /// another prefix with the same local name.
    fn qualified(&self, name: &str) -> &str {
        let exact = self.attr(name);
        if !exact.is_empty() {
            return exact;
        }
        let local = name.split_once(':').map_or(name, |(_, l)| l);
        for (k, v) in &self.attributes {
            let k_local = k.split_once(':').map_or(k.as_str(), |(_, l)| l);
            if k == name || k == local || k_local == local {
                return v.as_str();
            }
        }
        ""
    }

    fn text_content(&self) -> String {
        let mut out = String::new();
        self.append_text_content(&mut out);
        out
    }

    fn append_text_content(&self, out: &mut String) {
        for child in &self.children {
            match child {
                XmlNode::Text(text) => out.push_str(text),
                XmlNode::Element(el) => el.append_text_content(out),
            }
        }
    }

    fn has_timed_child(&self) -> bool {
        for child in &self.children {
            if let XmlNode::Element(el) = child
                && (!el.attr("begin").is_empty() || el.has_timed_child())
            {
                return true;
            }
        }
        false
    }
}

fn parse_xml(input: &str) -> Result<XmlElement, ()> {
    let trimmed = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let mut reader = Reader::from_str(trimmed);
    // Keep whitespace text nodes verbatim: spaces between `<span>` elements are
    // the word boundaries in Apple TTML.
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut roots: Vec<XmlElement> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(());
                }
                stack.push(XmlElement::from_start(&e)?);
            }
            Ok(Event::Empty(e)) => {
                let el = XmlElement::from_start(&e)?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XmlNode::Element(el));
                } else {
                    roots.push(el);
                }
            }
            Ok(Event::End(_)) => {
                let el = stack.pop().ok_or(())?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XmlNode::Element(el));
                } else {
                    roots.push(el);
                }
            }
            Ok(Event::Text(e)) => {
                let content = e.xml10_content();
                if let Some(parent) = stack.last_mut() {
                    parent.push_text(&content);
                } else if !content.trim().is_empty() {
                    return Err(());
                }
            }
            Ok(Event::CData(e)) => {
                let content = e.xml10_content();
                if let Some(parent) = stack.last_mut() {
                    parent.push_text(&content);
                } else if !content.trim().is_empty() {
                    return Err(());
                }
            }
            Ok(Event::GeneralRef(e)) => {
                let resolved = match e.resolve_char_ref().map_err(|_| ())? {
                    Some(ch) => {
                        let mut buf = [0u8; 4];
                        let s = ch.encode_utf8(&mut buf);
                        if let Some(parent) = stack.last_mut() {
                            parent.push_text(s);
                        } else {
                            return Err(());
                        }
                        continue;
                    }
                    None => resolve_xml_entity(e.as_ref()).ok_or(())?,
                };
                if let Some(parent) = stack.last_mut() {
                    parent.push_text(resolved);
                } else {
                    return Err(());
                }
            }
            Ok(Event::DocType(_)) => {
                // Refuse DTD declarations (mirrors disallow-doctype-decl).
                return Err(());
            }
            Ok(Event::Comment(_) | Event::Decl(_) | Event::PI(_)) => {}
            Ok(Event::Eof) => break,
            Err(_) => return Err(()),
        }
    }

    if !stack.is_empty() || roots.len() != 1 {
        return Err(());
    }
    roots.into_iter().next().ok_or(())
}

fn collect_elements_by_name<'a>(
    el: &'a XmlElement,
    target_name: &str,
    out: &mut Vec<&'a XmlElement>,
) {
    if el.name == target_name {
        out.push(el);
    }
    for child in &el.children {
        if let XmlNode::Element(child_el) = child {
            collect_elements_by_name(child_el, target_name, out);
        }
    }
}

fn agent_types(root: &XmlElement) -> HashMap<String, String> {
    let mut agents = Vec::new();
    collect_elements_by_name(root, "ttm:agent", &mut agents);
    if agents.is_empty() {
        collect_elements_by_name(root, "agent", &mut agents);
    }
    let mut types = HashMap::with_capacity(agents.len());
    for agent in agents {
        let id = agent.qualified("xml:id");
        let agent_type = agent.attr("type");
        if !id.is_empty() && !agent_type.is_empty() {
            types.insert(id.to_owned(), agent_type.to_owned());
        }
    }
    types
}

enum Piece {
    Text(String),
    Timed { text: String, start: i64, end: i64 },
}

fn line_from(paragraph: &XmlElement) -> Option<LyricLine> {
    let mut pieces = Vec::new();
    let mut backing_pieces = Vec::new();
    collect_pieces(paragraph, &mut pieces, &mut backing_pieces, false);

    let words = merge_into_words(&pieces);
    let backing_words = merge_into_words(&backing_pieces);
    let backing = (!backing_words.is_empty()).then(|| {
        let time_ms = backing_words[0].start_ms;
        let text = backing_words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Box::new(LyricLine {
            time_ms,
            text,
            words: backing_words,
            ..LyricLine::default()
        })
    });

    if words.is_empty() {
        // Line-synced TTML: a <p> with a stamp and bare text, no timed spans.
        let text = paragraph.text_content();
        let trimmed = text.trim();
        let begin = TtmlLyrics::time(Some(paragraph.attr("begin")))?;
        if trimmed.is_empty() {
            return None;
        }
        let end = TtmlLyrics::time(Some(paragraph.attr("end"))).filter(|&e| e > begin);
        return Some(LyricLine {
            time_ms: begin,
            text: trimmed.to_owned(),
            sung_until_ms: end,
            ..LyricLine::default()
        });
    }

    let first_start = words[0].start_ms;
    let begin = TtmlLyrics::time(Some(paragraph.attr("begin"))).unwrap_or(first_start);
    let text = words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Some(LyricLine {
        time_ms: begin.min(first_start),
        text,
        words,
        background: backing,
        ..LyricLine::default()
    })
}

fn collect_pieces(
    node: &XmlElement,
    out: &mut Vec<Piece>,
    backing: &mut Vec<Piece>,
    in_backing: bool,
) {
    for child in &node.children {
        match child {
            XmlNode::Element(el) => {
                let role = el.qualified("ttm:role");
                if SKIPPED_ROLES.contains(&role) {
                    continue;
                }
                let child_backing = in_backing || role == BACKGROUND_ROLE;
                let begin = TtmlLyrics::time(Some(el.attr("begin")));
                let end = TtmlLyrics::time(Some(el.attr("end")));
                if let (Some(start), Some(end)) = (begin, end)
                    && !el.has_timed_child()
                {
                    let piece = Piece::Timed {
                        text: el.text_content(),
                        start,
                        end,
                    };
                    if child_backing {
                        backing.push(piece);
                    } else {
                        out.push(piece);
                    }
                } else {
                    collect_pieces(el, out, backing, child_backing);
                }
            }
            XmlNode::Text(text) => {
                if !text.is_empty() {
                    let piece = Piece::Text(text.clone());
                    if in_backing {
                        backing.push(piece);
                    } else {
                        out.push(piece);
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct TtmlWordAcc {
    words: Vec<LyricWord>,
    current: String,
    current_chars: usize,
    syllables: Vec<LyricSyllable>,
    start: i64,
    end: i64,
    timed: bool,
}

impl TtmlWordAcc {
    fn flush(&mut self) {
        let text = self.current.trim().to_owned();
        self.current.clear();
        self.current_chars = 0;
        if !text.is_empty() && self.timed {
            let char_len = text.chars().count();
            let normalized = LyricSyllable::normalized(&self.syllables, char_len);
            self.words.push(LyricWord::with_syllables(
                self.start, self.end, text, normalized,
            ));
        }
        self.syllables.clear();
        self.timed = false;
    }
}

fn merge_into_words(pieces: &[Piece]) -> Vec<LyricWord> {
    let mut acc = TtmlWordAcc::default();

    for piece in pieces {
        match piece {
            Piece::Text(text) => {
                if text.trim().is_empty() {
                    acc.flush();
                } else if acc.timed {
                    // Trailing punctuation belongs to the word it follows.
                    acc.current.push_str(text);
                    acc.current_chars += text.chars().count();
                }
            }
            Piece::Timed {
                text,
                start: piece_start,
                end: piece_end,
            } => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if text.chars().next().is_some_and(char::is_whitespace) {
                    acc.flush();
                }
                if acc.current.is_empty() {
                    acc.start = *piece_start;
                }
                let from = acc.current_chars;
                acc.current.push_str(trimmed);
                acc.current_chars += trimmed.chars().count();
                acc.syllables.push(LyricSyllable::new(
                    *piece_start,
                    *piece_end,
                    from,
                    acc.current_chars,
                ));
                acc.end = *piece_end;
                acc.timed = true;
                if text.chars().next_back().is_some_and(char::is_whitespace) {
                    acc.flush();
                }
            }
        }
    }

    acc.flush();
    acc.words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::LyricAlignment;

    const SAMPLE_TTML: &str = r#"
        <tt xmlns="http://www.w3.org/ns/ttml" itunes:timing="Word" xml:lang="en">
          <body dur="3:21.570">
            <div begin="27.395" end="32.529" itunes:songPart="Verse">
              <p begin="27.395" end="28.960" itunes:key="L1" ttm:agent="v1">
                <span begin="27.395" end="27.549">I</span> <span begin="27.549" end="27.740">been</span> <span begin="27.740" end="28.077">tryna</span> <span begin="28.077" end="28.960">call</span>
              </p>
              <p begin="30.189" end="32.529" itunes:key="L2" ttm:agent="v1">
                <span begin="30.189" end="30.396">long</span> <span begin="31.839" end="31.996">e</span><span begin="31.996" end="32.529">nough</span>
              </p>
            </div>
          </body>
        </tt>
    "#;

    fn sung(lines: Vec<LyricLine>) -> Vec<LyricLine> {
        lines.into_iter().filter(|l| !l.is_gap()).collect()
    }

    #[test]
    fn reads_word_timings_and_merges_adjacent_syllable_spans() {
        let lines = sung(TtmlLyrics::parse(SAMPLE_TTML));
        assert_eq!(2, lines.len());
        assert_eq!("I been tryna call", lines[0].text);
        assert_eq!(27_395, lines[0].time_ms);
        assert!(lines[0].is_word_synced());
        assert_eq!(
            vec!["I", "been", "tryna", "call"],
            lines[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(27_549, lines[0].words[0].end_ms);
        assert_eq!(28_960, lines[0].end_ms());

        let line2 = &lines[1];
        assert_eq!("long enough", line2.text);
        assert_eq!(
            vec!["long", "enough"],
            line2
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert!(line2.words[0].syllables.is_empty());
        let enough = &line2.words[1];
        assert_eq!(31_839, enough.start_ms);
        assert_eq!(32_529, enough.end_ms);
        assert_eq!(
            vec![
                LyricSyllable::new(31_839, 31_996, 0, 1),
                LyricSyllable::new(31_996, 32_529, 1, 6),
            ],
            enough.syllables
        );

        // Sweep follows syllables rather than spreading evenly over the word.
        assert!((line2.revealed_chars(31_917) - 5.5).abs() <= 0.05);
        assert!((line2.revealed_chars(32_262) - 8.5).abs() <= 0.05);
        assert_eq!(vec![0..4, 5..6, 6..11], line2.sweep_spans());
    }

    #[test]
    fn skips_translations_and_hangs_background_vocals_under_lead() {
        let lines = sung(TtmlLyrics::parse(
            r#"
            <tt><body><div>
              <p begin="1.0" end="2.0">
                <span begin="1.0" end="2.0">hello</span>
                <span ttm:role="x-bg" begin="1.5" end="2.4"><span begin="1.5" end="2.4">(ooh)</span></span>
                <span ttm:role="x-translation" xml:lang="es">hola</span>
              </p>
            </div></body></tt>
            "#,
        ));
        assert_eq!(1, lines.len());
        let line = &lines[0];
        assert_eq!("hello", line.text);
        let bg = line.background.as_deref().expect("background present");
        assert_eq!("(ooh)", bg.text);
        assert_eq!(1_500, bg.time_ms);
        assert!(bg.is_word_synced());
        assert_eq!(2_400, line.end_ms());
    }

    #[test]
    fn reads_background_vocal_written_as_single_timed_span() {
        let lines = sung(TtmlLyrics::parse(
            r#"
            <tt><body><div>
              <p begin="1.0" end="2.0">
                <span begin="1.0" end="2.0">hello</span>
                <span ttm:role="x-bg" begin="1.5" end="2.0">(ooh)</span>
              </p>
            </div></body></tt>
            "#,
        ));
        assert_eq!(1, lines.len());
        assert_eq!("hello", lines[0].text);
        assert_eq!(
            Some("(ooh)"),
            lines[0].background.as_deref().map(|b| b.text.as_str())
        );
    }

    #[test]
    fn falls_back_to_plain_text_and_paragraph_end_for_line_synced_ttml() {
        let lines = sung(TtmlLyrics::parse(
            r#"<tt><body><div><p begin="00:12.50">just a line</p></div></body></tt>"#,
        ));
        assert_eq!(1, lines.len());
        assert_eq!("just a line", lines[0].text);
        assert_eq!(12_500, lines[0].time_ms);
        assert!(lines[0].words.is_empty());

        let with_gap = TtmlLyrics::parse(
            r#"
            <tt><body><div>
              <p begin="1.0" end="3.0">before the solo</p>
              <p begin="30.0" end="32.0">after the solo</p>
            </div></body></tt>
            "#,
        );
        assert_eq!(2, with_gap.iter().filter(|l| !l.is_gap()).count());
        let gaps: Vec<&LyricLine> = with_gap
            .iter()
            .filter(|l| l.is_gap() && l.time_ms > 0)
            .collect();
        assert_eq!(1, gaps.len());
        assert_eq!(3_000, gaps[0].time_ms);
    }

    #[test]
    fn lays_duet_out_on_alternating_sides_and_flips_all_right_songs() {
        let duet = r#"
            <tt xmlns="http://www.w3.org/ns/ttml">
              <head><metadata>
                <ttm:agent type="person" xml:id="v1"/>
                <ttm:agent type="person" xml:id="v2"/>
                <ttm:agent type="group" xml:id="v1000"/>
              </metadata></head>
              <body><div>
                <p begin="1.0" end="2.0" ttm:agent="v1">mine</p>
                <p begin="2.0" end="3.0" ttm:agent="v2">yours</p>
                <p begin="3.0" end="4.0" ttm:agent="v2">still yours</p>
                <p begin="4.0" end="5.0" ttm:agent="v1">mine again</p>
                <p begin="5.0" end="6.0" ttm:agent="v1000">both of us</p>
              </div></body>
            </tt>
        "#;
        let sides: Vec<LyricAlignment> = sung(TtmlLyrics::parse(duet))
            .into_iter()
            .map(|l| l.alignment)
            .collect();
        assert_eq!(
            vec![
                LyricAlignment::Start,
                LyricAlignment::End,
                LyricAlignment::End,
                LyricAlignment::Start,
                LyricAlignment::Start,
            ],
            sides
        );

        let single_voice: Vec<LyricAlignment> = sung(TtmlLyrics::parse(SAMPLE_TTML))
            .into_iter()
            .map(|l| l.alignment)
            .collect();
        assert_eq!(
            vec![LyricAlignment::Start, LyricAlignment::Start],
            single_voice
        );

        let all_other = r#"
            <tt><head><metadata><ttm:agent type="other" xml:id="v2"/></metadata></head>
            <body><div>
              <p begin="1.0" end="2.0" ttm:agent="v2">one</p>
              <p begin="2.0" end="3.0" ttm:agent="v2">two</p>
            </div></body></tt>
        "#;
        let flipped: Vec<LyricAlignment> = sung(TtmlLyrics::parse(all_other))
            .into_iter()
            .map(|l| l.alignment)
            .collect();
        assert_eq!(vec![LyricAlignment::Start, LyricAlignment::Start], flipped);
    }

    #[test]
    fn parses_lrc_red_blinding_lights_ttml_with_entities_and_echo() {
        let blinding_lights = r#"
            <tt xmlns="http://www.w3.org/ns/ttml" xmlns:lrc="http://lrc.red/lyric-ttml-internal"
             xmlns:ttm="http://www.w3.org/ns/ttml#metadata" lrc:timing="Word" xml:lang="en"><head><metadata>
            <ttm:agent type="person" xml:id="v1"><ttm:name type="full">Vocal 1</ttm:name></ttm:agent>
            <sourceMetadata xmlns="http://lrc.red/lyric-ttml-internal"><translations/>
            <audio lyricOffset="1.115" role="spatial"/></sourceMetadata></metadata></head><body dur="3:20.046">
            <div begin="2:19.610" end="2:23.809" lrc:songPart="Verse"><p begin="2:19.610" end="2:23.809" lrc:key="L27" ttm:agent="v1"><span begin="2:19.610" end="2:19.899">I&apos;m</span> <span begin="2:19.899" end="2:20.059">just</span> <span begin="2:20.059" end="2:20.353">calling</span> <span begin="2:20.353" end="2:20.726">back</span> <span begin="2:20.726" end="2:20.906">to</span> <span begin="2:20.906" end="2:21.228">let</span> <span begin="2:21.228" end="2:21.445">you</span> <span begin="2:21.445" end="2:22.261">know</span> <span ttm:role="x-bg"><span begin="2:21.720" end="2:22.154">(Back</span> <span begin="2:22.154" end="2:22.295">to</span> <span begin="2:22.295" end="2:22.706">let</span> <span begin="2:22.706" end="2:22.889">you</span> <span begin="2:22.889" end="2:23.809">know)</span></span></p></div>
            </body></tt>
        "#;
        let lines = sung(TtmlLyrics::parse(blinding_lights));
        assert_eq!(1, lines.len());
        let line = &lines[0];
        assert_eq!("I'm just calling back to let you know", line.text);
        assert!(line.is_word_synced());
        assert_eq!(139_610, line.time_ms);
        assert_eq!(
            Some("(Back to let you know)"),
            line.background.as_deref().map(|b| b.text.as_str())
        );
    }

    #[test]
    fn parses_every_ttml_clock_shape() {
        assert_eq!(Some(27_395), TtmlLyrics::time(Some("27.395")));
        assert_eq!(Some(65_200), TtmlLyrics::time(Some("1:05.20")));
        assert_eq!(Some(3_723_400), TtmlLyrics::time(Some("1:02:03.4")));
        assert_eq!(Some(1_500), TtmlLyrics::time(Some("1.5s")));
        assert_eq!(Some(250), TtmlLyrics::time(Some("250ms")));
        assert_eq!(None, TtmlLyrics::time(Some("")));
        assert_eq!(None, TtmlLyrics::time(None));
    }

    #[test]
    fn bad_xml_and_doctype_yield_empty() {
        assert!(TtmlLyrics::parse("<tt><body><p begin=").is_empty());
        assert!(
            TtmlLyrics::parse(
                r#"<!DOCTYPE tt [<!ENTITY xxe "bad">]><tt><body><p begin="1.0">&xxe;</p></body></tt>"#
            )
            .is_empty()
        );
    }
}
