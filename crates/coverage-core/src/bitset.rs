#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitSet {
    len: usize,
    words: Vec<u64>,
}

impl BitSet {
    pub fn new(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn words(&self) -> &[u64] {
        &self.words
    }
    pub fn words_mut(&mut self) -> &mut [u64] {
        &mut self.words
    }
    pub fn set(&mut self, index: usize) {
        assert!(index < self.len);
        self.words[index / 64] |= 1u64 << (index % 64);
    }
    pub fn contains(&self, index: usize) -> bool {
        index < self.len && self.words[index / 64] & (1u64 << (index % 64)) != 0
    }
    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
}

pub fn merge_counts<'a>(layers: impl IntoIterator<Item = &'a BitSet>, cells: usize) -> Vec<u8> {
    let mut output = vec![0u8; cells];
    for layer in layers {
        for (word_index, &source) in layer.words().iter().enumerate() {
            let mut word = source;
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                let index = word_index * 64 + bit;
                if index < cells {
                    output[index] = output[index].saturating_add(1);
                }
                word &= word - 1;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_merge_is_empty_coverage() {
        assert_eq!(merge_counts([], 3), vec![0; 3]);
    }
    #[test]
    fn merges_set_bits() {
        let mut a = BitSet::new(66);
        a.set(0);
        a.set(65);
        assert_eq!(merge_counts([&a, &a], 66)[65], 2);
    }
    #[test]
    fn saturates() {
        let mut a = BitSet::new(1);
        a.set(0);
        assert_eq!(merge_counts((0..300).map(|_| &a), 1), vec![255]);
    }
}
