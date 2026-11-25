//! Suggesting a tag for the pictures that look like the ones carrying it:
//! tag a few pictures of a character `#aria`, and the untagged pictures
//! of her come up as "probably #aria", to tag with a click or to turn
//! down ("not this", remembered so they do not come up again).
//!
//! Learned from the tag's own pictures, all of them together: a picture
//! is scored by how close it is to the few tagged pictures it is closest
//! to (`NEAREST`), so a character drawn in several outfits or poses is
//! matched by whichever of them it is like. How close is close enough is
//! learned from the tagged pictures too: each is scored against the rest
//! (leaving it out): a picture that scores as well as the typical one of
//! them is "likely". Tagged pictures often come from one board or one
//! artist and are closer to each other than to the same character drawn
//! elsewhere, so a picture most of the way there is still shown, as
//! "maybe" (`MAYBE`). A picture closer to one turned down for the tag than
//! to the tag's own pictures is left out.
//!
//! Plain numbers in and out (vectors of length 1, from
//! `canvas::picture_vectors`), so it can be measured on its own.

/// Tagged pictures a picture is compared with: its closest few.
const NEAREST: usize = 5;
/// Most pictures suggested at once.
pub const MOST: usize = 24;
/// Share of the tag's own pictures, scored against the rest, that a
/// suggested picture must score at least as well as: the typical one.
///
/// Measured 2026-11-18 on the user's 136 tags with 6+ pictures (30% of
/// each hidden, the rest as examples, 3 draws): a character tag
/// (`wonderwoman`, 10 pictures) got 92% of its suggestions right and 67%
/// of its hidden pictures back, the first suggestion right 3 of 3; the
/// artist tags, which are about style, about 21% right (judged against
/// pictures with other tags only), the first right 47% of the time.
/// Closer to the bar (0.8 here, 3 compared) listed more but fewer right.
const TYPICAL: f32 = 0.5;
/// With fewer tagged pictures than this, how close is close enough is
/// not learned from them but taken from how the candidates spread, as
/// for "pictures like this" (`similar::by_subject`).
const LEARN_FROM: usize = 3;
/// How far from the typical candidate towards the closest a picture must
/// be when the bar is not learned, for "likely" and for "maybe".
const SPREAD_CUT: f32 = 0.5;
const SPREAD_MAYBE: f32 = 0.35;
/// How far from the typical candidate up to the learned bar a picture
/// must be to be shown as "maybe". Measured 2026-11-18 on the user's
/// `#wonderwoman` (10 pictures from one board): learned bar 0.836, the
/// typical picture 0.656; the five closest untagged pictures, all of her,
/// at 0.809–0.833, other characters from 0.796 down. 0.8 puts the bar at
/// 0.800.
const MAYBE: f32 = 0.8;
/// Likeness at which two pictures are the same picture.
const SAME_PICTURE: f32 = 0.985;

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// How close `v` is to `tagged`: the mean likeness to its `NEAREST`
/// closest (fewer if there are fewer), leaving out `skip`.
fn closeness(v: &[f32], tagged: &[&[f32]], skip: Option<usize>) -> f32 {
    let mut likes: Vec<f32> = tagged.iter().enumerate().filter(|(i, _)| Some(*i) != skip).map(|(_, t)| dot(v, t)).collect();
    if likes.is_empty() {
        return f32::MIN;
    }
    likes.sort_by(|a, b| b.total_cmp(a));
    let n = likes.len().min(NEAREST);
    likes[..n].iter().sum::<f32>() / n as f32
}

/// Most of a tag's pictures scored against the rest to learn its bar: a
/// tag on hundreds of pictures is judged by an even spread of them.
const LEARN_MOST: usize = 40;

/// How close a picture must be to the tag's `tagged` pictures: as close
/// as the typical one of them is to the rest. `None` with too few.
fn learned_bar(tagged: &[&[f32]]) -> Option<f32> {
    if tagged.len() < LEARN_FROM {
        return None;
    }
    let step = tagged.len().div_ceil(LEARN_MOST);
    let mut own: Vec<f32> = (0..tagged.len()).step_by(step).map(|i| closeness(tagged[i], tagged, Some(i))).collect();
    own.sort_by(|a, b| b.total_cmp(a));
    Some(own[(((own.len() - 1) as f32) * TYPICAL).round() as usize])
}

/// How `v` stands with the tag its `tagged` pictures carry, if it reaches
/// "maybe": what the tag row offers for a picture. `sample` is a fair
/// sample of the pictures, to tell how close a typical one is. `None`
/// with too few tagged pictures to learn from. The app does it in two
/// halves (`tag_bars`, `fits_bars`), the slow one kept from one picture to
/// the next.
#[cfg(test)]
pub fn fits(tagged: &[&[f32]], v: &[f32], sample: &[&[f32]]) -> Option<Suggestion> {
    fits_bars(&tag_bars(tagged, sample)?, tagged, v)
}

/// The bars a picture must reach for the tag row to offer it the tag its
/// `tagged` pictures carry (`fits_bars`), from those pictures and `sample`,
/// a fair sample of all of them. The same for every picture, and the slow
/// part: each sample picture is scored against the tag's pictures. `None`
/// with too few tagged pictures to learn from.
pub fn tag_bars(tagged: &[&[f32]], sample: &[&[f32]]) -> Option<Bars> {
    let learned = learned_bar(tagged)?;
    let scored: Vec<f32> = sample.iter().map(|s| closeness(s, tagged, None)).collect();
    Some(bars_from(Some(learned), &scored))
}

/// How `v` stands with the tag its `tagged` pictures carry, given the tag's
/// bars (`tag_bars`), if it reaches "maybe".
pub fn fits_bars(b: &Bars, tagged: &[&[f32]], v: &[f32]) -> Option<Suggestion> {
    let close = closeness(v, tagged, None);
    (close >= b.maybe && close > b.median).then_some(Suggestion { id: 0, close: close.clamp(0.0, 1.0), likely: close >= b.likely })
}

/// The bars a candidate must reach to be "likely" and "maybe", from the
/// tag's pictures and the candidates' closeness to them (`scored`,
/// closest first, or any fair sample of the pictures).
fn bars(tagged: &[&[f32]], scored: &[f32]) -> Bars {
    bars_from(learned_bar(tagged), scored)
}

/// `bars`, with the tag's learned bar (`learned_bar`) already worked out.
fn bars_from(learned: Option<f32>, scored: &[f32]) -> Bars {
    let mut sorted = scored.to_vec();
    sorted.sort_by(|a, b| b.total_cmp(a));
    let top = sorted.first().copied().unwrap_or(0.0);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let spread = |cut: f32| median + cut * (top - median);
    match learned {
        Some(learned) => Bars { likely: learned, maybe: (median + MAYBE * (learned - median)).max(spread(SPREAD_MAYBE)).min(learned), median },
        None => Bars { likely: spread(SPREAD_CUT), maybe: spread(SPREAD_MAYBE), median },
    }
}

/// How close a picture must be to a tag's pictures to be "likely" and
/// "maybe", and how close the typical picture is (`bars`, `tag_bars`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bars {
    likely: f32,
    maybe: f32,
    median: f32,
}

/// A picture suggested for a tag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Suggestion {
    pub id: usize,
    /// How close it is to the tag's pictures, 0 to 1.
    pub close: f32,
    /// It reaches the bar learned from them; else it is a "maybe".
    pub likely: bool,
}

/// For finding out why a tag gets the suggestions it does: the "likely"
/// and "maybe" bars, the candidates' median closeness, and the closest few
/// candidates' ids and closeness.
#[cfg(test)]
pub fn explain<'v>(tagged: &[&[f32]], candidates: impl IntoIterator<Item = (usize, &'v [f32])>, few: usize) -> (f32, f32, f32, Vec<(usize, f32)>) {
    let mut scored: Vec<(usize, f32)> = candidates.into_iter().map(|(id, v)| (id, closeness(v, tagged, None))).collect();
    if scored.is_empty() || tagged.is_empty() {
        return (0.0, 0.0, 0.0, Vec::new());
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let b = bars(tagged, &scored.iter().map(|s| s.1).collect::<Vec<_>>());
    (b.likely, b.maybe, b.median, scored.into_iter().take(few).collect())
}

/// The pictures among `candidates` (an id and a vector each) that may
/// carry the tag its `tagged` pictures carry, closest first. `rejected`:
/// pictures turned down for the tag.
pub fn suggest<'v>(tagged: &[&[f32]], rejected: &[&[f32]], candidates: impl IntoIterator<Item = (usize, &'v [f32])>) -> Vec<Suggestion> {
    if tagged.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &[f32], f32)> = candidates.into_iter().map(|(id, v)| (id, v, closeness(v, tagged, None))).collect();
    if scored.is_empty() {
        return Vec::new();
    }
    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let b = bars(tagged, &scored.iter().map(|s| s.2).collect::<Vec<_>>());
    let mut kept: Vec<(usize, &[f32], f32)> = Vec::new();
    for (id, v, close) in scored {
        if close < b.maybe || close <= b.median || kept.len() == MOST {
            break;
        }
        if rejected.iter().any(|r| dot(v, r) >= close) || kept.iter().any(|k| dot(k.1, v) >= SAME_PICTURE) {
            continue;
        }
        kept.push((id, v, close));
    }
    kept.into_iter().map(|(id, _, close)| Suggestion { id, close: close.clamp(0.0, 1.0), likely: close >= b.likely }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vector of length 1: mostly `axis`, `lean` of the way to the next.
    fn at(axis: usize, lean: f32) -> Vec<f32> {
        let mut v = vec![0.0; 12];
        v[axis] = 1.0 - lean;
        v[(axis + 1) % 12] = lean;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    /// A vector of length 1 made of these `(axis, weight)`s.
    fn mix(parts: &[(usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.0; 12];
        for &(a, w) in parts {
            v[a] += w;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    #[test]
    fn suggests_pictures_like_the_tagged_ones_and_not_the_rest() {
        // The tag's pictures: axis 0, each with a touch of its own.
        let tagged = [mix(&[(0, 1.0), (1, 0.3)]), mix(&[(0, 1.0), (2, 0.3)]), mix(&[(0, 1.0), (3, 0.3)]), mix(&[(0, 1.0), (4, 0.3)])];
        let t: Vec<&[f32]> = tagged.iter().map(|v| v.as_slice()).collect();
        let cands = [
            mix(&[(0, 1.0), (5, 0.25)]),
            mix(&[(6, 1.0)]),
            mix(&[(0, 1.0), (1, 0.3), (2, 0.3)]),
            mix(&[(7, 1.0), (8, 0.2)]),
            mix(&[(9, 1.0)]),
            mix(&[(10, 1.0), (0, 0.3)]),
        ];
        let found = suggest(&t, &[], cands.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        let mut ids: Vec<usize> = found.iter().map(|f| f.id).collect();
        assert!(found[0].close >= found[1].close, "closest first");
        assert!(found.iter().all(|f| f.likely));
        ids.sort();
        assert_eq!(ids, [0, 2]);
    }

    #[test]
    fn a_turned_down_picture_keeps_its_like_away() {
        let tagged = [at(0, 0.0), at(0, 0.05), at(0, 0.1)];
        let t: Vec<&[f32]> = tagged.iter().map(|v| v.as_slice()).collect();
        // Leaning towards axis 1: closer to the turned-down one.
        let rejected = at(0, 0.45);
        let cands = [at(0, 0.03), at(0, 0.5), at(6, 0.0), at(8, 0.0), at(10, 0.0)];
        let without = suggest(&t, &[], cands.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        let with = suggest(&t, &[rejected.as_slice()], cands.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        assert!(with.iter().all(|f| f.id != 1), "{with:?}");
        assert!(with.iter().any(|f| f.id == 0));
        assert!(without.len() >= with.len());
    }

    #[test]
    fn a_picture_fits_a_tag_likely_or_maybe() {
        let tagged = [mix(&[(0, 1.0), (1, 0.3)]), mix(&[(0, 1.0), (2, 0.3)]), mix(&[(0, 1.0), (3, 0.3)])];
        let t: Vec<&[f32]> = tagged.iter().map(|v| v.as_slice()).collect();
        let others = [mix(&[(6, 1.0)]), mix(&[(7, 1.0)]), mix(&[(8, 1.0)]), mix(&[(9, 1.0), (0, 0.2)])];
        let sample: Vec<&[f32]> = others.iter().map(|v| v.as_slice()).collect();
        let close = fits(&t, &mix(&[(0, 1.0), (1, 0.2), (2, 0.2)]), &sample).expect("fits");
        assert!(close.likely);
        // Most of the way there: a maybe.
        let near = fits(&t, &mix(&[(0, 1.0), (5, 0.45)]), &sample).expect("fits");
        assert!(!near.likely, "{near:?}");
        assert!(fits(&t, &mix(&[(6, 1.0)]), &sample).is_none());
        assert!(fits(&t[..2], &tagged[0], &sample).is_none(), "two are too few to learn from");
    }

    #[test]
    fn a_tags_bars_serve_every_picture() {
        let tagged = [mix(&[(0, 1.0), (1, 0.3)]), mix(&[(0, 1.0), (2, 0.3)]), mix(&[(0, 1.0), (3, 0.3)]), mix(&[(0, 1.0), (4, 0.2)])];
        let t: Vec<&[f32]> = tagged.iter().map(|v| v.as_slice()).collect();
        let others = [mix(&[(6, 1.0)]), mix(&[(7, 1.0)]), mix(&[(8, 1.0)]), mix(&[(9, 1.0), (0, 0.2)]), mix(&[(0, 0.5), (5, 1.0)])];
        let sample: Vec<&[f32]> = others.iter().map(|v| v.as_slice()).collect();
        // Worked out once, they are the bars of the tag and sample.
        let b = tag_bars(&t, &sample).expect("enough to learn from");
        let scored: Vec<f32> = sample.iter().map(|s| closeness(s, &t, None)).collect();
        assert_eq!(b, bars(&t, &scored));
        // And they tell each picture what working them out for it would.
        let pictures = [mix(&[(0, 1.0), (1, 0.2), (2, 0.2)]), mix(&[(0, 1.0), (5, 0.45)]), mix(&[(6, 1.0)])];
        let close: Vec<f32> = pictures.iter().map(|p| closeness(p, &t, None)).collect();
        for (p, c) in pictures.iter().zip(close) {
            let want = (c >= b.maybe && c > b.median).then_some(Suggestion { id: 0, close: c.clamp(0.0, 1.0), likely: c >= b.likely });
            assert_eq!(fits_bars(&b, &t, p), want);
        }
        assert!(fits_bars(&b, &t, &pictures[0]).is_some_and(|s| s.likely));
        assert!(fits_bars(&b, &t, &pictures[2]).is_none());
        assert!(tag_bars(&t[..2], &sample).is_none(), "two are too few to learn from");
    }

    #[test]
    fn nothing_tagged_suggests_nothing() {
        let c = at(0, 0.0);
        assert!(suggest(&[], &[], [(0, c.as_slice())]).is_empty());
        let t = at(0, 0.0);
        assert!(suggest(&[t.as_slice()], &[], std::iter::empty()).is_empty());
    }
}
