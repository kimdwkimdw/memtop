#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub index: usize,
    pub area: Area,
}

pub fn layout(weights: &[u64], area: Area) -> Vec<Tile> {
    if weights.is_empty() || area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let mut items: Vec<(usize, u64)> = weights
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, weight)| *weight > 0)
        .collect();
    items.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    let mut tiles = Vec::with_capacity(items.len());
    split_items(&items, area, &mut tiles);
    tiles
}

fn split_items(items: &[(usize, u64)], area: Area, tiles: &mut Vec<Tile>) {
    if items.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }

    if items.len() == 1 || area.width == 1 && area.height == 1 {
        tiles.push(Tile {
            index: items[0].0,
            area,
        });
        return;
    }

    let total = items.iter().map(|(_, weight)| *weight).sum::<u64>();
    let split_at = balanced_split_index(items, total);
    let left_items = &items[..split_at];
    let right_items = &items[split_at..];

    if right_items.is_empty() {
        tiles.push(Tile {
            index: items[0].0,
            area,
        });
        return;
    }

    let left_weight = left_items.iter().map(|(_, weight)| *weight).sum::<u64>();

    if area.width >= area.height {
        let left_width = proportional_length(area.width, left_weight, total);
        let left_area = Area {
            width: left_width,
            ..area
        };
        let right_area = Area {
            x: area.x + left_width,
            width: area.width - left_width,
            ..area
        };
        split_items(left_items, left_area, tiles);
        split_items(right_items, right_area, tiles);
    } else {
        let top_height = proportional_length(area.height, left_weight, total);
        let top_area = Area {
            height: top_height,
            ..area
        };
        let bottom_area = Area {
            y: area.y + top_height,
            height: area.height - top_height,
            ..area
        };
        split_items(left_items, top_area, tiles);
        split_items(right_items, bottom_area, tiles);
    }
}

fn balanced_split_index(items: &[(usize, u64)], total: u64) -> usize {
    if items.len() <= 1 {
        return items.len();
    }

    let mut best_index = 1;
    let mut best_delta = u64::MAX;
    let mut running = 0;

    for index in 1..items.len() {
        running += items[index - 1].1;
        let left = running;
        let right = total.saturating_sub(running);
        let delta = left.abs_diff(right);
        if delta < best_delta {
            best_delta = delta;
            best_index = index;
        }
    }

    best_index
}

fn proportional_length(total_length: u16, part_weight: u64, total_weight: u64) -> u16 {
    if total_length <= 1 || total_weight == 0 {
        return total_length;
    }

    let length = ((total_length as u128 * part_weight as u128) / total_weight as u128) as u16;
    length.clamp(1, total_length - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_all_positive_weights() {
        let tiles = layout(
            &[10, 20, 30],
            Area {
                x: 0,
                y: 0,
                width: 60,
                height: 20,
            },
        );

        assert_eq!(tiles.len(), 3);
        assert!(tiles.iter().all(|tile| tile.area.width > 0));
        assert!(tiles.iter().all(|tile| tile.area.height > 0));
    }

    #[test]
    fn ignores_zero_weights() {
        let tiles = layout(
            &[0, 10, 0],
            Area {
                x: 0,
                y: 0,
                width: 10,
                height: 5,
            },
        );

        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].index, 1);
    }
}
