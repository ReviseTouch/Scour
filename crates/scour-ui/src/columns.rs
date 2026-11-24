//! The list's columns, and the widths they are given in a window of any size.

/// Which way a column's text sits in its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    /// Numbers, right-aligned so the digits line up down the column.
    End,
}

/// One column a window can show: the definition, not the drawing. Which columns
/// exist, their names, sort keys and starting widths are the same in every face.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// What the settings file calls it. Stable; the sort key is not.
    pub id: &'static str,
    /// The English source string, and therefore the catalogue key.
    pub msgid: &'static str,
    /// What `sort:` this column asks for; empty means it cannot be sorted by.
    pub sort: &'static str,
    /// What it asks for in pixels before anything is shared out or taken back.
    /// A drag overrides it, kept per column id in `scour_settings::widths`.
    pub width: u32,
    /// Never squeezed narrower than this: the floor is what keeps the date and
    /// the size on screen. A column on its floor stops paying, so the two text
    /// columns carry the rest — a path that elides is still a path.
    pub min: u32,
    /// Its share of the stretching room in a **narrow** window, as a percentage;
    /// zero never stretches and keeps [`width`](Column::width). The share slides
    /// to [`far`](Column::far) between [`NARROW`] and [`WIDE`], continuously, so
    /// the name leads in a narrow window and the location in a wide one.
    pub near: u8,
    /// Its share in a **wide** window, as a percentage. See [`near`](Column::near).
    pub far: u8,
    /// As wide as growth may take it; zero is no ceiling. Only the name has
    /// one: past it, what it declines goes round again to the location.
    pub max: u32,
    pub align: Align,
}

/// Every column, in default order. The first five are the default set; the rest
/// are there to be turned on.
pub const COLUMNS: &[Column] = &[
    Column {
        id: "name",
        msgid: "Name",
        sort: "name",
        width: 240,
        min: 110,
        near: 52,
        far: 30,
        max: 520,
        align: Align::Start,
    },
    Column {
        id: "kind",
        msgid: "Kind",
        sort: "kind",
        width: 110,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "path",
        msgid: "Location",
        sort: "path",
        width: 320,
        min: 120,
        near: 48,
        far: 70,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "mtime",
        msgid: "Modified",
        sort: "modified",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "size",
        msgid: "Size",
        sort: "size",
        width: 90,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::End,
    },
    Column {
        id: "ext",
        msgid: "Extension",
        sort: "ext",
        width: 80,
        min: 56,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "ctime",
        msgid: "Created",
        sort: "created",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "atime",
        msgid: "Accessed",
        sort: "accessed",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "perm",
        msgid: "Mode",
        sort: "mode",
        width: 108,
        min: 92,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "user",
        msgid: "Owner",
        sort: "uid",
        width: 100,
        min: 72,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "group",
        msgid: "Group",
        sort: "gid",
        width: 100,
        min: 72,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "disk",
        msgid: "On disk",
        sort: "disk",
        width: 90,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::End,
    },
];

/// What a window shows before anybody has chosen, in the reading order of the
/// question: what, where, when, how big. What kind is beside the name already,
/// as its icon in every face, so it has no column unless one is asked for.
pub const DEFAULT_COLUMNS: &[&str] = &["name", "path", "mtime", "size"];

/// Look one up by the id the settings file uses.
pub fn column(id: &str) -> Option<&'static Column> {
    COLUMNS.iter().find(|c| c.id == id)
}

/// The window width at which a stretching column asks for its [`Column::near`]
/// share, and below which it asks for nothing more: under 700px the floors are
/// already doing the deciding.
pub const NARROW: u32 = 700;

/// And where it asks for its [`Column::far`] share, and stops changing. A
/// laptop leaves near a thousand pixels, so the slide covers the working range.
pub const WIDE: u32 = 1900;

/// How wide each of the columns on screen is, given the room there is. The
/// widths add up to `avail` exactly at every size, which is the property the
/// whole thing exists for; only a room narrower than one pixel a column fails.
///
/// **A dragged column is exactly as wide as it was dragged**, whichever it is:
/// the stretching columns nobody dragged take what is left, sharing it by a
/// share that slides from [`Column::near`] to [`Column::far`], each up to its
/// [`Column::max`]. When that does not fit, room is taken back in turn: from
/// the undragged stretching columns, down to their floors; then from the
/// dragged ones, so a drag stops where the rest are on their floors; then
/// from every column. Below [`floor_width`] the floors give way together
/// rather than draw a column off the right-hand edge.
///
/// `chosen` is the width a person dragged that column to, or `None`. Zero
/// counts as `None`, as the settings file means it.
pub fn lay_out(ids: &[&str], chosen: impl Fn(&str) -> Option<u32>, avail: u32) -> Vec<u32> {
    let cols: Vec<&Column> = ids.iter().filter_map(|id| column(id)).collect();
    if cols.is_empty() {
        return Vec::new();
    }
    let set: Vec<Option<u32>> = cols
        .iter()
        .map(|c| chosen(c.id).filter(|w| *w > 0))
        .collect();

    // What each asks for: a drag, else its own width; never under its floor.
    let mut w: Vec<u32> = cols
        .iter()
        .zip(&set)
        .map(|(c, s)| s.unwrap_or(c.width).max(c.min))
        .collect();
    // The stretching columns nobody dragged share what everything else left.
    let free: Vec<usize> = (0..cols.len())
        .filter(|&i| cols[i].near > 0 && set[i].is_none())
        .collect();
    let taken: u32 = (0..cols.len())
        .filter(|i| !free.contains(i))
        .map(|i| w[i])
        .sum();
    let budget = avail.saturating_sub(taken);
    let weight: u32 = free.iter().map(|&i| share_at(cols[i], avail)).sum();
    let mut handed = 0;
    for (n, &i) in free.iter().enumerate() {
        // The last takes what the others left, so the shares add up to the
        // budget exactly however they rounded.
        let want = if n + 1 == free.len() {
            budget.saturating_sub(handed)
        } else {
            (budget * share_at(cols[i], avail))
                .checked_div(weight)
                .unwrap_or(0)
        };
        handed += want;
        w[i] = want.max(cols[i].min);
        if cols[i].max > 0 {
            w[i] = w[i].min(cols[i].max).max(cols[i].min);
        }
    }

    let sum: u32 = w.iter().sum();
    if sum < avail {
        share_out(&cols, &set, &mut w, avail - sum);
    } else if sum > avail {
        let dragged: Vec<usize> = (0..cols.len())
            .filter(|&i| cols[i].near > 0 && set[i].is_some())
            .collect();
        let every: Vec<usize> = (0..cols.len()).collect();
        let mut over = sum - avail;
        for from in [&free, &dragged, &every] {
            over = take_back(&cols, &mut w, over, from);
        }
        squash(&mut w, avail);
    }
    w
}

/// What share of the stretching room this column asks for at this width, in
/// parts per thousand. Straight-line and integer the whole way, because the
/// browser page has to reach the same number.
fn share_at(c: &Column, avail: u32) -> u32 {
    let t = if avail <= NARROW {
        0
    } else if avail >= WIDE {
        1000
    } else {
        (avail - NARROW) * 1000 / (WIDE - NARROW)
    };
    (u32::from(c.near) * 10 * (1000 - t) + u32::from(c.far) * 10 * t) / 1000
}

/// The narrowest these columns can be drawn with every floor still honoured.
/// Below it, [`lay_out`] still fits them in, but under the widths they need.
pub fn floor_width(ids: &[&str]) -> u32 {
    ids.iter().filter_map(|id| column(id)).map(|c| c.min).sum()
}

/// Move 2: hand out what is still spare, and let a column that fills up pass.
fn share_out(cols: &[&Column], set: &[Option<u32>], w: &mut [u32], spare: u32) {
    let mut left = spare;
    loop {
        // Recomputed each round, because a column that reached its ceiling is
        // no longer open and its share has to go somewhere.
        let open: Vec<usize> = (0..cols.len())
            .filter(|&i| {
                set[i].is_none() && cols[i].near > 0 && (cols[i].max == 0 || w[i] < cols[i].max)
            })
            .collect();
        let weight: u32 = open.iter().map(|&i| u32::from(cols[i].far).max(1)).sum();
        if left == 0 {
            return;
        }
        // Every stretching column dragged or at its ceiling: the last of them
        // takes the rest, or a strip of empty row shows past the last column.
        if weight == 0 {
            if let Some(i) = (0..cols.len()).rev().find(|&i| cols[i].near > 0) {
                w[i] += left;
            }
            return;
        }
        let mut spent = 0;
        for (n, &i) in open.iter().enumerate() {
            // The last one takes the rounding too, or a hairline of panel
            // shows past the last column.
            let share = if n + 1 == open.len() {
                left - spent
            } else {
                left * u32::from(cols[i].far).max(1) / weight
            };
            let before = w[i];
            let want = before + share;
            w[i] = if cols[i].max > 0 {
                want.min(cols[i].max)
            } else {
                want
            };
            spent += w[i] - before;
        }
        // Every open column filled up and none could take the remainder;
        // another round would find the same thing.
        if spent == 0 {
            return;
        }
        left -= spent;
    }
}

/// Move 4, and only when move 3 ran out of room: everything in proportion. The
/// floors say which column gives way first, not that the window can keep them —
/// 400px for five floors adding to 466 has no ordering that works.
fn squash(w: &mut [u32], avail: u32) {
    let sum: u32 = w.iter().sum();
    // Not a window: below a pixel a column there is nothing to say.
    if sum <= avail || sum == 0 || avail < w.len() as u32 {
        return;
    }
    // The widest takes the rounding, because a pixel matters least to it.
    let widest = (0..w.len()).max_by_key(|&i| w[i]).unwrap_or(0);
    let mut spent = 0;
    for (i, x) in w.iter_mut().enumerate() {
        if i == widest {
            continue;
        }
        *x = (u64::from(*x) * u64::from(avail) / u64::from(sum)).max(1) as u32;
        spent += *x;
    }
    w[widest] = avail.saturating_sub(spent).max(1);
}

/// Move 5, where a face knows what its rows hold: **a column wider than
/// anything in it gives the difference to one that is cutting its text.** A
/// list of short names under long locations otherwise kept half the row empty
/// beside the names while the locations elided.
///
/// `need` is the widest text the column has to show, in the same pixels as
/// `w`, or `None` where nobody measured. Only the stretching columns nobody
/// dragged give or take: a drag is kept exactly, here as in [`lay_out`]. What
/// moves is what one side can spare and the other can use, whichever is less,
/// so the widths still add up to what they did; no column goes under its
/// floor, and room nobody is short of stays where it was.
pub fn fit_to(
    ids: &[&str],
    chosen: impl Fn(&str) -> Option<u32>,
    need: impl Fn(&str) -> Option<u32>,
    w: &mut [u32],
) {
    let cols: Vec<&Column> = ids.iter().filter_map(|id| column(id)).collect();
    if cols.len() != w.len() {
        return;
    }
    let mut spare = vec![0u32; w.len()];
    let mut short = vec![0u32; w.len()];
    for (i, c) in cols.iter().enumerate() {
        if c.near == 0 || chosen(c.id).is_some_and(|x| x > 0) {
            continue;
        }
        let Some(n) = need(c.id) else { continue };
        let keep = n.max(c.min);
        if w[i] > keep {
            spare[i] = w[i] - keep;
        } else if n > w[i] {
            short[i] = n - w[i];
            // A ceiling holds against this too: past it, the name elides.
            if c.max > 0 {
                short[i] = short[i].min(c.max.saturating_sub(w[i]));
            }
        }
    }
    let have: u32 = spare.iter().sum();
    let want: u32 = short.iter().sum();
    let moved = have.min(want);
    if moved == 0 {
        return;
    }
    // Each side in proportion, the last of each taking the rounding, so what
    // leaves is exactly what arrives.
    let shift = |amounts: &[u32], total: u32| -> Vec<u32> {
        let at: Vec<usize> = (0..amounts.len()).filter(|&i| amounts[i] > 0).collect();
        let mut out = vec![0u32; amounts.len()];
        let mut handed = 0;
        for &i in &at {
            out[i] = (u64::from(moved) * u64::from(amounts[i]) / u64::from(total)) as u32;
            handed += out[i];
        }
        // What rounding left over goes to whoever still has room for it;
        // `moved` is at most each side's total, so it always fits.
        for &i in &at {
            let more = (moved - handed).min(amounts[i] - out[i]);
            out[i] += more;
            handed += more;
        }
        out
    };
    let give = shift(&spare, have);
    let take = shift(&short, want);
    for i in 0..w.len() {
        w[i] = w[i] - give[i] + take[i];
    }
}

/// Take `over` back from the columns in `from`, in proportion to what each has
/// above its floor. Returns what could not be taken from them.
fn take_back(cols: &[&Column], w: &mut [u32], over: u32, from: &[usize]) -> u32 {
    let mut left = over;
    loop {
        let open: Vec<usize> = from
            .iter()
            .copied()
            .filter(|&i| w[i] > cols[i].min)
            .collect();
        let room: u32 = open.iter().map(|&i| w[i] - cols[i].min).sum();
        // Every one of them on its floor: the next tier, or `squash`, goes on.
        if left == 0 || room == 0 {
            return left;
        }
        let take = left.min(room);
        let mut taken = 0;
        for (n, &i) in open.iter().enumerate() {
            let want = if n + 1 == open.len() {
                take - taken
            } else {
                take * (w[i] - cols[i].min) / room
            };
            let share = want.min(w[i] - cols[i].min);
            w[i] -= share;
            taken += share;
        }
        if taken == 0 {
            return left;
        }
        left -= taken;
    }
}

#[cfg(test)]
mod column_tests;
