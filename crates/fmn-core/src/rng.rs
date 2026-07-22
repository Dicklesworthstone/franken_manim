//! The one RNG (§6.5, D-06, BN-01): PCG64DXSM with named substreams and
//! keyed per-frame forks.
//!
//! The Reference carries two seeded legacy streams (CPython `random` +
//! NumPy legacy `RandomState`); with output parity dropped, both are
//! irrelevant. FrankenManim standardizes on **PCG64DXSM seeded through
//! NumPy's `SeedSequence`**, bit-exact against NumPy for explicit seeds —
//! locked by `fixtures/rng_vectors.txt` (generated from NumPy itself by
//! `scripts/gen_rng_vectors.py`).
//!
//! **Named substreams:** every subsystem draws from
//! `root.substream("name")`, derived via a spawn key, so features never
//! perturb each other's sequences — adding a consumer cannot shift an
//! existing consumer's draws. The canonical name encoding (mirrored in
//! the generator script) is `[byte_len] ++ utf8 bytes packed LE into u32
//! words, zero-padded`.
//!
//! **Keyed per-frame forks (Rev 4, §9.5):** a frame's stream derives from
//! `(substream, frame_index)` — never from sequential pulls consumed in
//! scheduler completion order. `fork_frame(k)` is a pure function of its
//! key: identical whatever the call order or thread, which is what makes
//! frame-parallel rendering replay-identical *by construction*. Per-thread
//! completion-order RNG is on §10.5's permanent refusal list (D-18).
//!
//! **Snapshot/restore:** [`Pcg64Dxsm::state`]/[`Pcg64Dxsm::restore`]
//! round-trip the full generator state for SceneState and the replay
//! journal.
//!
//! **Ordered-iteration audit note:** anything render-affecting that
//! iterates a map must use an ordered map ([`OrderedMap`]) — hash-map
//! iteration order would smuggle nondeterminism past the RNG discipline.
//!
//! Ownership note (D-4): the RNG belongs to franken_numpy (`fnp-random`,
//! already bit-exact to NumPy). This owned implementation carries the
//! contract until SUITE.lock (fm-g2c) makes fnp consumable; the migration
//! bead swaps the internals, the API and the vectors stay.

/// Render-affecting map iteration must be ordered (§6.5). Use this alias
/// so the intent audits greppably; `HashMap` iteration in render-affecting
/// paths is a review error.
pub type OrderedMap<K, V> = std::collections::BTreeMap<K, V>;

const XSHIFT: u32 = 16;
const INIT_A: u32 = 0x43b0_d7e5;
const MULT_A: u32 = 0x931e_8875;
const INIT_B: u32 = 0x8b51_f9dd;
const MULT_B: u32 = 0x58f3_8ded;
const MIX_MULT_L: u32 = 0xca01_f9dd;
const MIX_MULT_R: u32 = 0x4973_f715;
const POOL_SIZE: usize = 4;

/// NumPy's `SeedSequence`: an entropy pool with the O'Neill hash mixing,
/// bit-exact (locked by the `seedseq` vectors).
#[derive(Debug, Clone)]
pub struct SeedSequence {
    pool: [u32; POOL_SIZE],
}

fn hashmix(value: u32, hash_const: &mut u32) -> u32 {
    let mut value = value ^ *hash_const;
    *hash_const = hash_const.wrapping_mul(MULT_A);
    value = value.wrapping_mul(*hash_const);
    value ^= value >> XSHIFT;
    value
}

fn mix(x: u32, y: u32) -> u32 {
    let mut result = x
        .wrapping_mul(MIX_MULT_L)
        .wrapping_sub(y.wrapping_mul(MIX_MULT_R));
    result ^= result >> XSHIFT;
    result
}

/// A u64 seed as NumPy coerces integers: little-endian u32 words (zero is
/// the single word `[0]`).
fn entropy_words(seed: u64) -> Vec<u32> {
    if seed == 0 {
        return vec![0];
    }
    let mut words = Vec::new();
    let mut value = seed;
    while value != 0 {
        words.push(value as u32);
        value >>= 32;
    }
    words
}

impl SeedSequence {
    /// A root sequence from a u64 seed (NumPy: `SeedSequence(seed)`).
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self::assemble(&entropy_words(seed), &[])
    }

    /// A child sequence (NumPy: `SeedSequence(entropy=seed, spawn_key=key)`).
    #[must_use]
    pub fn with_spawn_key(seed: u64, spawn_key: &[u32]) -> Self {
        Self::assemble(&entropy_words(seed), spawn_key)
    }

    fn assemble(entropy: &[u32], spawn_key: &[u32]) -> Self {
        let mut assembled: Vec<u32> = entropy.to_vec();
        if !spawn_key.is_empty() && assembled.len() < POOL_SIZE {
            assembled.resize(POOL_SIZE, 0);
        }
        assembled.extend_from_slice(spawn_key);

        let mut pool = [0u32; POOL_SIZE];
        let mut hash_const = INIT_A;
        for (i, slot) in pool.iter_mut().enumerate() {
            let value = assembled.get(i).copied().unwrap_or(0);
            *slot = hashmix(value, &mut hash_const);
        }
        for i_src in 0..POOL_SIZE {
            for i_dst in 0..POOL_SIZE {
                if i_src != i_dst {
                    let hashed = hashmix(pool[i_src], &mut hash_const);
                    pool[i_dst] = mix(pool[i_dst], hashed);
                }
            }
        }
        for &extra in assembled.iter().skip(POOL_SIZE) {
            for slot in pool.iter_mut() {
                let hashed = hashmix(extra, &mut hash_const);
                *slot = mix(*slot, hashed);
            }
        }
        Self { pool }
    }

    /// `generate_state(n)` as u32 words.
    #[must_use]
    pub fn generate_state(&self, n_words: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(n_words);
        let mut hash_const = INIT_B;
        for i in 0..n_words {
            let mut value = self.pool[i % POOL_SIZE];
            value ^= hash_const;
            hash_const = hash_const.wrapping_mul(MULT_B);
            value = value.wrapping_mul(hash_const);
            value ^= value >> XSHIFT;
            out.push(value);
        }
        out
    }

    /// `generate_state(n, np.uint64)`: pairs of u32 words, low word first
    /// (NumPy's little-endian view).
    #[must_use]
    pub fn generate_state_u64(&self, n_words: usize) -> Vec<u64> {
        let words = self.generate_state(2 * n_words);
        words
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u64::from(pair[0]) | (u64::from(pair[1]) << 32))
            .collect()
    }
}

/// The cheap-multiplier constant shared by the state transition and the
/// DXSM output function.
const CHEAP_MULTIPLIER: u64 = 0xda94_2042_e4dd_58b5;

/// PCG's default 128-bit multiplier — used only by the seeding dance
/// (NumPy's PCG64DXSM runs the *standard* `pcg_setseq_128_srandom_r`,
/// then transitions with the cheap multiplier at runtime; the parity
/// vectors pin this asymmetry).
const DEFAULT_MULTIPLIER: u128 = 0x2360_ed05_1fc6_5da4_4385_df64_9fcc_f645;

/// NumPy's PCG64DXSM bit generator: 128-bit LCG state with the DXSM
/// output permutation. Draw-for-draw identical to
/// `np.random.Generator(np.random.PCG64DXSM(seed))` (locked by the
/// `draws`/`doubles` vectors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcg64Dxsm {
    state: u128,
    inc: u128,
}

impl Pcg64Dxsm {
    /// Seed from a `SeedSequence` exactly as NumPy's `PCG64DXSM.__init__`:
    /// four u64 words → (initstate, initseq) → the PCG srandom dance.
    #[must_use]
    pub fn from_seed_sequence(seq: &SeedSequence) -> Self {
        let words = seq.generate_state_u64(4);
        let initstate = (u128::from(words[0]) << 64) | u128::from(words[1]);
        let initseq = (u128::from(words[2]) << 64) | u128::from(words[3]);
        let mut rng = Self {
            state: 0,
            inc: (initseq << 1) | 1,
        };
        rng.seed_step();
        rng.state = rng.state.wrapping_add(initstate);
        rng.seed_step();
        rng
    }

    /// The srandom transition (full multiplier — seeding only).
    fn seed_step(&mut self) {
        self.state = self
            .state
            .wrapping_mul(DEFAULT_MULTIPLIER)
            .wrapping_add(self.inc);
    }

    /// Convenience: seed like `PCG64DXSM(seed)`.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self::from_seed_sequence(&SeedSequence::from_seed(seed))
    }

    fn step(&mut self) {
        self.state = self
            .state
            .wrapping_mul(u128::from(CHEAP_MULTIPLIER))
            .wrapping_add(self.inc);
    }

    /// The next raw u64 (DXSM output of the current state, then step).
    pub fn next_u64(&mut self) -> u64 {
        let lo = (self.state as u64) | 1;
        let mut hi = (self.state >> 64) as u64;
        hi ^= hi >> 32;
        hi = hi.wrapping_mul(CHEAP_MULTIPLIER);
        hi ^= hi >> 48;
        hi = hi.wrapping_mul(lo);
        self.step();
        hi
    }

    /// The next double in `[0, 1)`, NumPy's `random()`: 53 bits over 2^53.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Full state snapshot `((state_hi, state_lo), (inc_hi, inc_lo))` for
    /// SceneState and the replay journal.
    #[must_use]
    pub fn state(&self) -> ([u64; 2], [u64; 2]) {
        (
            [(self.state >> 64) as u64, self.state as u64],
            [(self.inc >> 64) as u64, self.inc as u64],
        )
    }

    /// Restore a snapshot taken with [`Pcg64Dxsm::state`].
    #[must_use]
    pub fn restore(state: [u64; 2], inc: [u64; 2]) -> Self {
        Self {
            state: (u128::from(state[0]) << 64) | u128::from(state[1]),
            inc: (u128::from(inc[0]) << 64) | u128::from(inc[1]),
        }
    }
}

/// The canonical substream-name encoding (mirrored in
/// `scripts/gen_rng_vectors.py`): `[byte_len] ++ utf8 packed LE, padded`.
fn name_words(name: &str) -> Vec<u32> {
    let bytes = name.as_bytes();
    let mut words = vec![u32::try_from(bytes.len()).expect("substream name over 4 GiB")];
    for chunk in bytes.chunks(4) {
        let mut padded = [0u8; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(padded));
    }
    words
}

/// The scene's root RNG: one seed, many named substreams.
#[derive(Debug, Clone)]
pub struct RngRoot {
    seed: u64,
}

impl RngRoot {
    /// Seeded construction — the deterministic path. Unseeded scenes get
    /// their entropy from fmn-platform's capability layer (never from
    /// direct OS access here; fmn-core does no I/O) and then use this.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self { seed }
    }

    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The named substream handle for a subsystem.
    #[must_use]
    pub fn substream(&self, name: &str) -> Substream {
        Substream {
            seed: self.seed,
            key: name_words(name),
        }
    }
}

/// A named substream: its own sequential generator plus keyed per-frame
/// forks. Distinct names are independent by construction — adding one
/// never shifts another's draws.
#[derive(Debug, Clone)]
pub struct Substream {
    seed: u64,
    key: Vec<u32>,
}

impl Substream {
    /// The substream's own sequential generator (scene-serial use:
    /// layout jitter, shuffles).
    #[must_use]
    pub fn sequential(&self) -> Pcg64Dxsm {
        Pcg64Dxsm::from_seed_sequence(&SeedSequence::with_spawn_key(self.seed, &self.key))
    }

    /// The keyed per-frame fork (§9.5): a pure function of
    /// `(substream, frame_index)`. Call it from any thread in any order —
    /// the stream is identical, which is what frame-parallel purity
    /// requires (D-18).
    #[must_use]
    pub fn fork_frame(&self, frame_index: u64) -> Pcg64Dxsm {
        let mut key = self.key.clone();
        key.push(frame_index as u32);
        key.push((frame_index >> 32) as u32);
        Pcg64Dxsm::from_seed_sequence(&SeedSequence::with_spawn_key(self.seed, &key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substream_independence() {
        let root = RngRoot::from_seed(7);
        let a_before: Vec<u64> = {
            let mut g = root.substream("alpha").sequential();
            (0..8).map(|_| g.next_u64()).collect()
        };
        // Drawing (a lot) from another substream…
        let mut other = root.substream("beta").sequential();
        for _ in 0..10_000 {
            other.next_u64();
        }
        // …and adding a brand-new consumer…
        let _new = root.substream("gamma").sequential();
        // …leaves alpha's sequence untouched.
        let a_after: Vec<u64> = {
            let mut g = root.substream("alpha").sequential();
            (0..8).map(|_| g.next_u64()).collect()
        };
        assert_eq!(a_before, a_after);
    }

    #[test]
    fn fork_is_call_order_invariant() {
        let root = RngRoot::from_seed(99);
        let sub = root.substream("frames");
        let forward: Vec<u64> = (0..64).map(|k| sub.fork_frame(k).next_u64()).collect();
        let mut backward: Vec<u64> = (0..64)
            .rev()
            .map(|k| sub.fork_frame(k).next_u64())
            .collect();
        backward.reverse();
        assert_eq!(forward, backward);
        // Interleaved / repeated forks agree too.
        assert_eq!(sub.fork_frame(31).next_u64(), forward[31]);
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let mut g = Pcg64Dxsm::from_seed(1234);
        for _ in 0..17 {
            g.next_u64();
        }
        let (state, inc) = g.state();
        let mut restored = Pcg64Dxsm::restore(state, inc);
        for _ in 0..32 {
            assert_eq!(g.next_u64(), restored.next_u64());
        }
    }

    #[test]
    fn doubles_are_53_bit() {
        let mut g = Pcg64Dxsm::from_seed(5);
        for _ in 0..1000 {
            let x = g.next_f64();
            assert!((0.0..1.0).contains(&x));
        }
    }
}
