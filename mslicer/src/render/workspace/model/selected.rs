pub struct Selected {
    words: Vec<u32>,
}

impl Selected {
    pub fn new(faces: usize) -> Self {
        Selected {
            words: vec![0; faces.div_ceil(32)],
        }
    }

    pub fn into_inner(self) -> Vec<u32> {
        self.words
    }
}
