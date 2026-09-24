//! BM25F scoring for the graph-free source search.
//!
//! Every searched file is a document with a few fields: the file-name parts,
//! the directory parts, the names of the definitions it contains, its code
//! lines and its comment lines. A request term's frequency in each field is
//! length-normalized against that field's average length, weighted, and
//! summed into one pseudo-frequency, which is then saturated once
//! (Robertson and Zaragoza's BM25F). Inverse document frequency comes from
//! the same pass over the searched files.
//!
//! Length normalization is what keeps a large hub file, which mentions every
//! term somewhere, from outranking the small file that is about them.

/// A document field. The discriminant indexes [`FIELDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    /// Parts of the file name without its extension (`retry_policy.rs` ->
    /// `retry`, `policy`).
    Name = 0,
    /// Parts of the directory names.
    Dir = 1,
    /// Parts of the names of the definitions the file contains.
    Symbol = 2,
    /// Identifier tokens on code lines.
    Code = 3,
    /// Identifier tokens on comment lines.
    Comment = 4,
}

pub(super) const FIELD_COUNT: usize = 5;

/// Per-field weight and length-normalization strength.
#[derive(Debug, Clone, Copy)]
pub(super) struct FieldParams {
    pub weight: f64,
    pub b: f64,
}

/// Field parameters, indexed by [`Field`]. Names and definitions say what a
/// file is about, so a match there counts for more than one in running text.
pub(super) const FIELDS: [FieldParams; FIELD_COUNT] = [
    FieldParams {
        weight: 3.0,
        b: 0.75,
    },
    FieldParams {
        weight: 1.0,
        b: 0.75,
    },
    FieldParams {
        weight: 2.0,
        b: 0.75,
    },
    FieldParams {
        weight: 1.0,
        b: 0.75,
    },
    FieldParams {
        weight: 1.0,
        b: 0.75,
    },
];

/// Term-frequency saturation.
pub(super) const K1: f64 = 1.2;

/// One file's term frequencies and field lengths.
#[derive(Debug, Clone, Default)]
pub(super) struct Document {
    /// `tf[term][field]`.
    pub tf: Vec<[f64; FIELD_COUNT]>,
    pub len: [f64; FIELD_COUNT],
}

impl Document {
    pub fn new(terms: usize) -> Self {
        Document {
            tf: vec![[0.0; FIELD_COUNT]; terms],
            len: [0.0; FIELD_COUNT],
        }
    }

    pub fn add(&mut self, term: usize, field: Field, count: f64) {
        self.tf[term][field as usize] += count;
    }

    /// Whether `term` occurs in any field.
    pub fn contains(&self, term: usize) -> bool {
        self.tf[term].iter().any(|tf| *tf > 0.0)
    }
}

/// Collection statistics for one search.
#[derive(Debug, Clone)]
pub(super) struct Corpus {
    pub idf: Vec<f64>,
    /// Average length of each field over the searched files where it is
    /// non-empty (a file without definitions says nothing about how many a
    /// file that has them usually holds).
    pub avg_len: [f64; FIELD_COUNT],
}

impl Corpus {
    /// Statistics over `documents`, the files that match some term, and
    /// `lengths`, the field lengths of every searched file (the rest hold no
    /// term, but they are still part of the collection).
    ///
    /// Average lengths come from every searched file, not only the matching
    /// ones: a large file mentions more terms, so it is overrepresented among
    /// the matches, and averaging over them alone would raise the average
    /// and let hub files escape length normalization.
    pub fn new(documents: &[Document], terms: usize, lengths: &[[f64; FIELD_COUNT]]) -> Self {
        let n = lengths.len().max(documents.len()).max(1) as f64;
        let idf = (0..terms)
            .map(|term| {
                let df = documents.iter().filter(|doc| doc.contains(term)).count() as f64;
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
            })
            .collect();
        let mut avg_len = [1.0; FIELD_COUNT];
        for (field, average) in avg_len.iter_mut().enumerate() {
            let (sum, count) = lengths
                .iter()
                .map(|len| len[field])
                .filter(|len| *len > 0.0)
                .fold((0.0, 0usize), |(sum, count), len| (sum + len, count + 1));
            if count > 0 {
                *average = (sum / count as f64).max(1.0);
            }
        }
        Corpus { idf, avg_len }
    }

    /// The weighted, length-normalized frequency of `term` across fields.
    fn pseudo_tf(&self, document: &Document, term: usize) -> f64 {
        (0..FIELD_COUNT)
            .map(|field| {
                let tf = document.tf[term][field];
                if tf <= 0.0 {
                    return 0.0;
                }
                let params = FIELDS[field];
                let norm = 1.0 - params.b + params.b * document.len[field] / self.avg_len[field];
                params.weight * tf / norm.max(f64::EPSILON)
            })
            .sum()
    }

    pub fn term_score(&self, document: &Document, term: usize) -> f64 {
        let tf = self.pseudo_tf(document, term);
        if tf <= 0.0 {
            return 0.0;
        }
        self.idf[term] * tf * (K1 + 1.0) / (tf + K1)
    }

    pub fn score(&self, document: &Document) -> f64 {
        (0..self.idf.len())
            .map(|term| self.term_score(document, term))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(terms: usize, entries: &[(usize, Field, f64)], lens: [f64; FIELD_COUNT]) -> Document {
        let mut document = Document::new(terms);
        for &(term, field, count) in entries {
            document.add(term, field, count);
        }
        document.len = lens;
        document
    }

    fn lengths(docs: &[Document]) -> Vec<[f64; FIELD_COUNT]> {
        docs.iter().map(|doc| doc.len).collect()
    }

    const CODE_ONLY: [f64; FIELD_COUNT] = [1.0, 1.0, 0.0, 100.0, 0.0];

    #[test]
    fn rare_terms_weigh_more_than_common_ones() {
        // Term 0 is in every document, term 1 in one.
        let docs = vec![
            doc(
                2,
                &[(0, Field::Code, 1.0), (1, Field::Code, 1.0)],
                CODE_ONLY,
            ),
            doc(2, &[(0, Field::Code, 1.0)], CODE_ONLY),
            doc(2, &[(0, Field::Code, 1.0)], CODE_ONLY),
            doc(2, &[(0, Field::Code, 1.0)], CODE_ONLY),
        ];
        let corpus = Corpus::new(&docs, 2, &lengths(&docs));
        assert!(corpus.idf[1] > corpus.idf[0] * 2.0, "{:?}", corpus.idf);
        assert!(corpus.term_score(&docs[0], 1) > corpus.term_score(&docs[0], 0));
    }

    #[test]
    fn length_normalization_ranks_a_focused_file_above_a_hub() {
        // The hub mentions both terms more often, but is 40x longer.
        let focused = doc(
            2,
            &[(0, Field::Code, 3.0), (1, Field::Code, 2.0)],
            [1.0, 1.0, 0.0, 60.0, 0.0],
        );
        let hub = doc(
            2,
            &[(0, Field::Code, 9.0), (1, Field::Code, 6.0)],
            [1.0, 1.0, 0.0, 2_400.0, 0.0],
        );
        let filler = (0..20).map(|_| doc(2, &[], [1.0, 1.0, 0.0, 200.0, 0.0]));
        let docs = [focused, hub].into_iter().chain(filler).collect::<Vec<_>>();
        let corpus = Corpus::new(&docs, 2, &lengths(&docs));
        assert!(
            corpus.score(&docs[0]) > corpus.score(&docs[1]),
            "focused {} hub {}",
            corpus.score(&docs[0]),
            corpus.score(&docs[1])
        );
    }

    #[test]
    fn a_name_or_definition_match_outweighs_the_same_count_in_code() {
        let lens = [2.0, 2.0, 4.0, 100.0, 0.0];
        let docs = vec![
            doc(1, &[(0, Field::Code, 1.0)], lens),
            doc(1, &[(0, Field::Name, 1.0)], lens),
            doc(1, &[(0, Field::Symbol, 1.0)], lens),
            doc(1, &[], lens),
            doc(1, &[], lens),
        ];
        let corpus = Corpus::new(&docs, 1, &lengths(&docs));
        let code = corpus.score(&docs[0]);
        let name = corpus.score(&docs[1]);
        let symbol = corpus.score(&docs[2]);
        assert!(name > symbol && symbol > code, "{name} {symbol} {code}");
    }

    #[test]
    fn frequency_saturates() {
        let lens = [1.0, 1.0, 0.0, 100.0, 0.0];
        let docs = vec![
            doc(1, &[(0, Field::Code, 1.0)], lens),
            doc(1, &[(0, Field::Code, 100.0)], lens),
            doc(1, &[], lens),
        ];
        let corpus = Corpus::new(&docs, 1, &lengths(&docs));
        let once = corpus.score(&docs[0]);
        let many = corpus.score(&docs[1]);
        assert!(many > once);
        assert!(many < corpus.idf[0] * (K1 + 1.0), "bounded by idf*(k1+1)");
    }

    #[test]
    fn average_length_counts_files_that_match_nothing() {
        // The hub is the only long file among many short ones. Among the
        // matching files alone it would set the average itself and escape
        // length normalization; over the whole collection it is 20x the
        // average and scores below the short file that is about the term.
        let short = [1.0, 1.0, 0.0, 50.0, 0.0];
        let hub_len = [1.0, 1.0, 0.0, 5_000.0, 0.0];
        let focused = doc(1, &[(0, Field::Code, 2.0)], short);
        let hub = doc(1, &[(0, Field::Code, 20.0)], hub_len);
        let docs = vec![focused, hub];
        let mut population = vec![short, hub_len];
        population.extend(std::iter::repeat_n(short, 98));
        let corpus = Corpus::new(&docs, 1, &population);
        assert!(
            corpus.avg_len[Field::Code as usize] < 110.0,
            "{:?}",
            corpus.avg_len
        );
        assert!(
            corpus.score(&docs[0]) > corpus.score(&docs[1]),
            "focused {} hub {}",
            corpus.score(&docs[0]),
            corpus.score(&docs[1])
        );
        // Averaged over the matches only, the hub would win.
        let matches_only = Corpus::new(&docs, 1, &lengths(&docs));
        assert!(matches_only.score(&docs[1]) > matches_only.score(&docs[0]));
    }
}
