//! Cuts a piece of the video stream into datagrams and adds spare ones, so that the piece arrives whole although
//! some of its datagrams get lost ("forward error correction"). The client rebuilds what is missing from the
//! spares (see index.html), which costs no round trip.
//!
//! A piece is cut into shards of equal length, in blocks of at most `BLOCK`. A block of k shards gets m spares,
//! and any k of those k + m datagrams give the block back (a Reed-Solomon code): spare j is the sum over the
//! shards i of shard i times 1 / (i + (255 - j)), calculated byte by byte in the field of 256 elements, where
//! adding is exclusive or. How many spares a block gets goes by how much the link loses: few shards need
//! more of them for their number than many do, and a link that loses nothing needs hardly any.

use crate::ws;

/// Every datagram of a piece begins with: the kind (video), the piece's number (4 bytes), its length (3), how
/// many shards of the piece come before this block (2), the block's k and m, and which of its k + m this is.
pub const HEAD: usize = 13;
/// The most shards in a block. Larger blocks ride out longer runs of loss, but making their spares takes time
/// in proportion to the block's size.
const BLOCK: usize = 64;
/// A block gets as many spares as it takes for it to go missing less often than this, where each datagram is
/// lost by chance. At 60 frames a second that is a frame every few minutes, which costs a moment's standstill.
const GOES_MISSING: f64 = 1e-4;

/// Products in the field of 256 elements that x^8 + x^4 + x^3 + x^2 + 1 defines, and each element's inverse.
static FIELD: ([[u8; 256]; 256], [u8; 256]) = field();

const fn field() -> ([[u8; 256]; 256], [u8; 256]) {
    // The powers of 2 run through every element but 0, which gives logarithms to multiply by.
    let (mut power, mut logarithm) = ([0u8; 510], [0usize; 256]);
    let (mut exponent, mut value) = (0, 1u16);
    while exponent < 510 {
        power[exponent] = value as u8;
        if exponent < 255 {
            logarithm[value as usize] = exponent;
        }
        value <<= 1;
        if value > 255 {
            value ^= 0x11D;
        }
        exponent += 1;
    }
    let (mut products, mut inverses) = ([[0u8; 256]; 256], [0u8; 256]);
    let mut a = 1;
    while a < 256 {
        inverses[a] = power[255 - logarithm[a]];
        let mut b = 1;
        while b < 256 {
            products[a][b] = power[logarithm[a] + logarithm[b]];
            b += 1;
        }
        a += 1;
    }
    (products, inverses)
}

/// What shard `shard` of a block is multiplied by in its spare `spare`.
fn factor(spare: usize, shard: usize) -> u8 {
    FIELD.1[shard ^ (255 - spare)]
}

/// Adds `factor` times `from` to `to`.
fn add(to: &mut [u8], from: &[u8], factor: u8) {
    let products = &FIELD.0[factor as usize];
    for (to, from) in to.iter_mut().zip(from) {
        *to ^= products[*from as usize];
    }
}

/// The `count` spares of a block's shards.
fn spares(shards: &[&[u8]], count: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|spare| {
            let mut sum = vec![0; shards[0].len()];
            for (index, shard) in shards.iter().enumerate() {
                add(&mut sum, shard, factor(spare, index));
            }
            sum
        })
        .collect()
}

/// How likely more than `most` of `count` datagrams get lost, where each is with likelihood `loss`.
fn more_lost(count: usize, most: usize, loss: f64) -> f64 {
    // The likelihood that exactly none is lost, then one, and so on.
    let mut exactly = (1.0 - loss).powi(count as i32);
    let mut up_to = 0.0;
    for lost in 0..=most {
        up_to += exactly;
        exactly *= (count - lost) as f64 / (lost + 1) as f64 * loss / (1.0 - loss);
    }
    (1.0 - up_to).max(0.0)
}

/// How many spares a block of `k` shards gets on a link that loses the share `loss` of its datagrams.
fn spares_needed(k: usize, loss: f64) -> usize {
    let loss = loss.clamp(0.0, 0.5);
    (1..BLOCK).find(|&m| more_lost(k + m, m, loss) < GOES_MISSING).unwrap_or(BLOCK)
}

/// The datagrams that carry piece `number` where a datagram has `room` bytes, with the spares it takes on a
/// link that loses the share `loss` of them.
pub fn cut(number: u32, piece: &[u8], room: usize, loss: f64) -> Vec<Vec<u8>> {
    // As few shards as fit, all of the same length, the last filled up with zeros.
    let count = piece.len().div_ceil(room - HEAD).max(1);
    let length = piece.len().div_ceil(count).max(1);
    let mut padded = piece.to_vec();
    padded.resize(count * length, 0);
    let shards: Vec<&[u8]> = padded.chunks(length).collect();
    let blocks = count.div_ceil(BLOCK);
    let mut datagrams = Vec::new();
    let mut before = 0;
    for block in 0..blocks {
        let k = count / blocks + usize::from(block < count % blocks);
        let m = spares_needed(k, loss);
        let block = &shards[before..before + k];
        let spares = spares(block, m);
        for (index, shard) in block.iter().copied().chain(spares.iter().map(Vec::as_slice)).enumerate() {
            let mut datagram = Vec::with_capacity(HEAD + length);
            datagram.push(ws::VIDEO);
            datagram.extend(number.to_be_bytes());
            datagram.extend(&(piece.len() as u32).to_be_bytes()[1..]);
            datagram.extend((before as u16).to_be_bytes());
            datagram.extend([k as u8, m as u8, index as u8]);
            datagram.extend(shard);
            datagrams.push(datagram);
        }
        before += k;
    }
    datagrams
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the client does: puts a piece together again from the datagrams that arrived, if they suffice.
    fn join(datagrams: &[&Vec<u8>]) -> Option<Vec<u8>> {
        let first = datagrams.first()?;
        let piece_length = u32::from_be_bytes([0, first[5], first[6], first[7]]) as usize;
        let length = first.len() - HEAD;
        let mut piece = vec![0; piece_length.div_ceil(length).max(1) * length];
        let mut covered = 0;
        let mut bases: Vec<usize> =
            datagrams.iter().map(|datagram| u16::from_be_bytes([datagram[8], datagram[9]]).into()).collect();
        bases.sort();
        bases.dedup();
        for base in bases {
            let of_block: Vec<&&Vec<u8>> = datagrams
                .iter()
                .filter(|datagram| usize::from(u16::from_be_bytes([datagram[8], datagram[9]])) == base)
                .collect();
            let (k, m) = (of_block[0][10] as usize, of_block[0][11] as usize);
            let mut shards: Vec<Option<Vec<u8>>> = vec![None; k + m];
            for datagram in of_block {
                shards[datagram[12] as usize] = Some(datagram[HEAD..].to_vec());
            }
            let missing: Vec<usize> = (0..k).filter(|&index| shards[index].is_none()).collect();
            let spares: Vec<usize> = (0..m).filter(|&spare| shards[k + spare].is_some()).take(missing.len()).collect();
            if spares.len() < missing.len() {
                return None;
            }
            // Each spare, less what the shards that are there contribute to it, is a sum of the missing ones:
            // as many equations as unknowns, solved by elimination.
            let mut sums: Vec<Vec<u8>> = spares
                .iter()
                .map(|&spare| {
                    let mut sum = shards[k + spare].clone().unwrap();
                    for index in (0..k).filter(|index| !missing.contains(index)) {
                        add(&mut sum, shards[index].as_ref().unwrap(), factor(spare, index));
                    }
                    sum
                })
                .collect();
            let mut factors: Vec<Vec<u8>> =
                spares.iter().map(|&spare| missing.iter().map(|&index| factor(spare, index)).collect()).collect();
            for column in 0..missing.len() {
                let pivot = (column..missing.len()).find(|&row| factors[row][column] != 0).unwrap();
                factors.swap(column, pivot);
                sums.swap(column, pivot);
                let scale = FIELD.1[factors[column][column] as usize];
                factors[column] =
                    factors[column].iter().map(|&factor| FIELD.0[scale as usize][factor as usize]).collect();
                sums[column] = sums[column].iter().map(|&byte| FIELD.0[scale as usize][byte as usize]).collect();
                for row in (0..missing.len()).filter(|&row| row != column) {
                    let times = factors[row][column];
                    let (pivot_factors, pivot_sum) = (factors[column].clone(), sums[column].clone());
                    add(&mut factors[row], &pivot_factors, times);
                    add(&mut sums[row], &pivot_sum, times);
                }
            }
            for (&index, sum) in missing.iter().zip(sums) {
                shards[index] = Some(sum);
            }
            for (index, shard) in shards.into_iter().take(k).enumerate() {
                piece[(base + index) * length..][..length].copy_from_slice(&shard.unwrap());
            }
            covered += k;
        }
        (covered * length >= piece_length).then(|| piece[..piece_length].to_vec())
    }

    fn noise(length: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9u32;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn the_field_multiplies_and_inverts() {
        assert_eq!(FIELD.0[2][0x80], 0x1D, "2 * x^7 wraps around by the polynomial");
        for element in 1..=255u8 {
            assert_eq!(FIELD.0[element as usize][FIELD.1[element as usize] as usize], 1);
        }
    }

    #[test]
    fn a_piece_survives_the_loss_of_as_many_datagrams_as_it_has_spares() {
        for length in [0, 1, 700, 1187, 1188, 30_000, 200_000] {
            let piece = noise(length);
            let datagrams = cut(7, &piece, 1200, 0.05);
            assert!(datagrams.iter().all(|datagram| datagram.len() <= 1200 && datagram.len() == datagrams[0].len()));
            assert_eq!(
                join(&datagrams.iter().collect::<Vec<_>>()).as_ref(),
                Some(&piece),
                "{length} bytes, nothing lost"
            );
            // From each block, lose as many as it has spares: the first of its shards, then every third datagram.
            let mut left_to_lose = std::collections::HashMap::new();
            let arrived: Vec<&Vec<u8>> = datagrams
                .iter()
                .enumerate()
                .filter(|(position, datagram)| {
                    let left = left_to_lose.entry((datagram[8], datagram[9])).or_insert(datagram[11]);
                    let lose = *left > 0 && (datagram[12] == 0 || position % 3 == 0);
                    *left -= u8::from(lose);
                    !lose
                })
                .map(|(_, datagram)| datagram)
                .collect();
            assert!(arrived.len() < datagrams.len());
            assert_eq!(join(&arrived).as_ref(), Some(&piece), "{length} bytes, as many lost as there are spares");
        }
    }

    #[test]
    fn a_piece_that_lost_more_than_its_spares_is_not_made_up() {
        let piece = noise(5000);
        let datagrams = cut(7, &piece, 1200, 0.05);
        assert_eq!(datagrams.len(), 5 + 4);
        assert_eq!(join(&datagrams.iter().skip(4).collect::<Vec<_>>()).as_ref(), Some(&piece));
        assert_eq!(join(&datagrams.iter().skip(5).collect::<Vec<_>>()), None);
    }

    #[test]
    fn spares_go_by_the_loss_and_by_how_few_shards_there_are() {
        let needed = |loss: f64| [1, 5, 20, 64].map(|k| spares_needed(k, loss));
        assert_eq!(needed(0.0), [1, 1, 1, 1]);
        assert_eq!(needed(0.01), [1, 2, 3, 5]);
        assert_eq!(needed(0.05), [3, 4, 7, 12]);
        assert_eq!(needed(0.9), needed(0.5), "a link that loses more than half is beyond spares");
        // A large piece is several blocks, each with its own.
        let datagrams = cut(7, &noise(200_000), 1200, 0.05);
        let shards = 200_000usize.div_ceil(1200 - HEAD);
        assert_eq!(datagrams.len(), shards + shards.div_ceil(BLOCK) * 12);
    }
}
