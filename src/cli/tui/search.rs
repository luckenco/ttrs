use std::cmp::Reverse;

use nucleo_matcher::{self as nucleo, pattern};
use tt::model;

/// A fixed picker catalog with reusable matcher scratch space and ranked results.
/// Filtering is synchronous; redraws and selection changes only read the results.
pub(super) struct Matches<T> {
    items: Vec<(T, nucleo::Utf32String)>,
    ranked: Vec<(u32, usize)>,
    matcher: nucleo::Matcher,
}

pub(super) type Projects = Matches<model::ProjectSummary>;

impl Projects {
    pub fn new(projects: Vec<model::ProjectSummary>) -> Self {
        Self::from_labels(projects.into_iter().map(|project| {
            let label = format!(
                "{} {} {}",
                project.project.name(),
                project.client.name(),
                project.agreement.name()
            );
            (project, label)
        }))
    }
}

impl<T> Matches<T> {
    pub fn from_labels(items: impl IntoIterator<Item = (T, String)>) -> Self {
        let items: Vec<_> = items
            .into_iter()
            .map(|(item, label)| (item, label.into()))
            .collect();
        let ranked = (0..items.len()).map(|i| (0, i)).collect();
        Self {
            items,
            ranked,
            matcher: nucleo::Matcher::default(),
        }
    }

    /// Case-insensitive fuzzy words without pattern operators.
    /// Best scores come first; ties and empty queries preserve catalog order.
    pub fn filter(&mut self, query: &str) {
        let pattern = pattern::Pattern::new(
            query,
            pattern::CaseMatching::Ignore,
            pattern::Normalization::Smart,
            pattern::AtomKind::Fuzzy,
        );
        self.ranked.clear();
        self.ranked.extend(
            self.items
                .iter()
                .enumerate()
                .filter_map(|(index, (_, label))| {
                    pattern
                        .score(label.slice(..), &mut self.matcher)
                        .map(|score| (score, index))
                }),
        );
        self.ranked.sort_by_key(|&(score, _)| Reverse(score));
    }

    pub fn matches(&self) -> impl ExactSizeIterator<Item = &T> {
        self.ranked.iter().map(|&(_, index)| &self.items[index].0)
    }
}
