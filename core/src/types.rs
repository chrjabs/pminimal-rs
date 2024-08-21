//! # Types
//!
//! Shared types for the $P$-minimal solver.

use std::ops::{Index, Range};

use rustsat::{
    encodings::{card, pb, CollectClauses},
    instances::ManageVars,
    types::{Assignment, Lit, LitIter, RsHashMap, WLitIter},
};

/// The Pareto front of an instance. This is the return type of the solver.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParetoFront<S = Assignment>
where
    S: Clone + Eq,
{
    ndoms: Vec<NonDomPoint<S>>,
}

impl<S> ParetoFront<S>
where
    S: Clone + Eq,
{
    /// Converts all solutions to another type
    pub fn convert_solutions<C, S2>(self, conv: &mut C) -> ParetoFront<S2>
    where
        S2: Clone + Eq,
        C: FnMut(S) -> S2,
    {
        ParetoFront {
            ndoms: self
                .ndoms
                .into_iter()
                .map(|pp| pp.convert_solutions(conv))
                .collect(),
        }
    }

    /// Gets the number of non-dominated points
    pub fn len(&self) -> usize {
        self.ndoms.len()
    }

    /// Checks if the Pareto front is empty
    pub fn is_empty(&self) -> bool {
        self.ndoms.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, NonDomPoint<S>> {
        self.ndoms.iter()
    }
}

impl<S: Clone + Eq> Index<usize> for ParetoFront<S> {
    type Output = NonDomPoint<S>;

    fn index(&self, index: usize) -> &Self::Output {
        &self.ndoms[index]
    }
}

impl<'a, S> IntoIterator for &'a ParetoFront<S>
where
    S: Clone + Eq,
{
    type Item = &'a NonDomPoint<S>;

    type IntoIter = std::slice::Iter<'a, NonDomPoint<S>>;

    fn into_iter(self) -> Self::IntoIter {
        self.ndoms.iter()
    }
}

impl<S> IntoIterator for ParetoFront<S>
where
    S: Clone + Eq,
{
    type Item = NonDomPoint<S>;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.ndoms.into_iter()
    }
}

impl<S> Extend<NonDomPoint<S>> for ParetoFront<S>
where
    S: Clone + Eq,
{
    fn extend<T: IntoIterator<Item = NonDomPoint<S>>>(&mut self, iter: T) {
        #[cfg(all(debug_assertions, feature = "check-non-dominance"))]
        {
            let cost_set = rustsat::types::RsHashSet::from_iter(
                self.ndoms.iter().map(|nd| nd.costs().clone()),
            );
            let check_dominated = |c1: &Vec<isize>, c2: &Vec<isize>| -> bool {
                let mut dom = 0;
                for (c1, c2) in c1.iter().zip(c2.iter()) {
                    if c1 < c2 {
                        if dom <= 0 {
                            dom = -1;
                        } else {
                            return false;
                        }
                    } else if c2 < c1 {
                        if dom >= 0 {
                            dom = 1;
                        } else {
                            return false;
                        }
                    }
                }
                return dom != 0;
            };
            for ndom in iter.into_iter() {
                for cost in &cost_set {
                    debug_assert!(!check_dominated(ndom.costs(), cost));
                }
                debug_assert!(!cost_set.contains(ndom.costs()));
                self.ndoms.push(ndom);
            }
            return;
        }
        #[cfg(not(all(debug_assertions, feature = "check-non-dominance")))]
        self.ndoms.extend(iter)
    }
}

/// A point on the Pareto front. This is a point in _objective_ space, i.e., a
/// tuple of costs. Multiple Pareto-optimal solutions can be associated with one
/// non-dominated point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonDomPoint<S = Assignment>
where
    S: Clone + Eq,
{
    costs: Vec<isize>,
    sols: Vec<S>,
}

impl<S> NonDomPoint<S>
where
    S: Clone + Eq,
{
    /// Constructs a new non-dominated point
    pub(crate) fn new(mut costs: Vec<isize>) -> Self {
        costs.shrink_to_fit();
        NonDomPoint {
            costs,
            sols: vec![],
        }
    }

    /// Adds a solution to the non-dominated point
    pub(crate) fn add_sol(&mut self, sol: S) {
        self.sols.push(sol)
    }

    /// Gets the number of solutions in the non-dominated point
    pub fn n_sols(&self) -> usize {
        self.sols.len()
    }

    /// Converts all solutions to another type
    pub fn convert_solutions<C, S2>(self, conv: &mut C) -> NonDomPoint<S2>
    where
        S2: Clone + Eq,
        C: FnMut(S) -> S2,
    {
        NonDomPoint {
            costs: self.costs,
            sols: self.sols.into_iter().map(conv).collect(),
        }
    }

    /// Gets the costs of the non-dominated point
    pub fn costs(&self) -> &Vec<isize> {
        &self.costs
    }

    /// Gets an iterator over references to the solutions
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.sols.iter()
    }
}

impl<'a, S> IntoIterator for &'a NonDomPoint<S>
where
    S: Clone + Eq,
{
    type Item = &'a S;

    type IntoIter = std::slice::Iter<'a, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.sols.iter()
    }
}

impl<S> IntoIterator for NonDomPoint<S>
where
    S: Clone + Eq,
{
    type Item = S;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.sols.into_iter()
    }
}

/// Data regarding an objective
pub(crate) enum Objective {
    Weighted {
        offset: isize,
        lits: RsHashMap<Lit, usize>,
    },
    Unweighted {
        offset: isize,
        unit_weight: usize,
        lits: Vec<Lit>,
    },
    Constant {
        offset: isize,
    },
}

impl Objective {
    /// Initializes the objective from a soft lit iterator and an offset
    pub fn new<Iter: WLitIter>(lits: Iter, offset: isize) -> Self {
        let lits: Vec<_> = lits.into_iter().collect();
        if lits.is_empty() {
            return Objective::Constant { offset };
        }
        let unit_weight = lits[0].1;
        let weighted = 'detect_weighted: {
            for (_, w) in &lits {
                if *w != unit_weight {
                    break 'detect_weighted true;
                }
            }
            false
        };
        if weighted {
            Objective::Weighted {
                offset,
                lits: lits.into_iter().collect(),
            }
        } else {
            Objective::Unweighted {
                offset,
                unit_weight,
                lits: lits.into_iter().map(|(l, _)| l).collect(),
            }
        }
    }

    /// Gets the offset of the encoding
    pub fn offset(&self) -> isize {
        match self {
            Objective::Weighted { offset, .. } => *offset,
            Objective::Unweighted { offset, .. } => *offset,
            Objective::Constant { offset } => *offset,
        }
    }

    /// Unified iterator over encodings
    pub fn iter(&self) -> ObjIter<'_> {
        match self {
            Objective::Weighted { lits, .. } => ObjIter::Weighted(lits.iter()),
            Objective::Unweighted { lits, .. } => ObjIter::Unweighted(lits.iter()),
            Objective::Constant { .. } => ObjIter::Constant,
        }
    }
}

pub(crate) enum ObjIter<'a> {
    Weighted(std::collections::hash_map::Iter<'a, Lit, usize>),
    Unweighted(std::slice::Iter<'a, Lit>),
    Constant,
}

impl Iterator for ObjIter<'_> {
    type Item = (Lit, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            ObjIter::Weighted(iter) => iter.next().map(|(&l, &w)| (l, w)),
            ObjIter::Unweighted(iter) => iter.next().map(|&l| (l, 1)),
            ObjIter::Constant => None,
        }
    }
}

/// An objective encoding for either a weighted or an unweighted objective
pub(crate) enum ObjEncoding<PBE, CE> {
    Weighted(PBE, usize),
    Unweighted(CE, usize),
    Constant,
}

impl<PBE, CE> ObjEncoding<PBE, CE>
where
    PBE: pb::BoundUpperIncremental + FromIterator<(Lit, usize)>,
{
    /// Initializes a new objective encoding for a weighted objective
    pub fn new_weighted<VM: ManageVars, LI: WLitIter>(
        lits: LI,
        reserve: bool,
        var_manager: &mut VM,
    ) -> Self {
        let mut encoding = PBE::from_iter(lits);
        if reserve {
            encoding.reserve(var_manager);
        }
        ObjEncoding::Weighted(encoding, 0)
    }
}

impl<PBE, CE> ObjEncoding<PBE, CE>
where
    CE: card::BoundUpperIncremental + FromIterator<Lit>,
{
    /// Initializes a new objective encoding for a weighted objective
    pub fn new_unweighted<VM: ManageVars, LI: LitIter>(
        lits: LI,
        reserve: bool,
        var_manager: &mut VM,
    ) -> Self {
        let mut encoding = CE::from_iter(lits);
        if reserve {
            encoding.reserve(var_manager);
        }
        ObjEncoding::Unweighted(encoding, 0)
    }
}

impl<PBE, CE> ObjEncoding<PBE, CE> {
    /// Gets the offset of the encoding
    pub fn offset(&self) -> usize {
        match self {
            ObjEncoding::Weighted(_, offset) => *offset,
            ObjEncoding::Unweighted(_, offset) => *offset,
            ObjEncoding::Constant => 0,
        }
    }
}

impl<PBE, CE> ObjEncoding<PBE, CE>
where
    PBE: pb::BoundUpperIncremental,
    CE: card::BoundUpperIncremental,
{
    /// Gets the next higher objective value
    pub fn next_higher(&self, val: usize) -> usize {
        match self {
            ObjEncoding::Weighted(enc, offset) => enc.next_higher(val - offset) + offset,
            ObjEncoding::Unweighted(..) => val + 1,
            ObjEncoding::Constant => val,
        }
    }

    /// Encodes the given range
    pub fn encode_ub_change<Col>(
        &mut self,
        range: Range<usize>,
        collector: &mut Col,
        var_manager: &mut dyn ManageVars,
    ) -> Result<(), rustsat::OutOfMemory>
    where
        Col: CollectClauses,
    {
        match self {
            ObjEncoding::Weighted(enc, offset) => enc.encode_ub_change(
                if range.start >= *offset {
                    range.start - *offset
                } else {
                    0
                }..if range.end >= *offset {
                    range.end - *offset
                } else {
                    0
                },
                collector,
                var_manager,
            ),
            ObjEncoding::Unweighted(enc, offset) => enc.encode_ub_change(
                if range.start >= *offset {
                    range.start - *offset
                } else {
                    0
                }..if range.end >= *offset {
                    range.end - *offset
                } else {
                    0
                },
                collector,
                var_manager,
            ),
            ObjEncoding::Constant => Ok(()),
        }
    }

    /// Enforces the given upper bound
    pub fn enforce_ub(&mut self, ub: usize) -> Result<Vec<Lit>, rustsat::encodings::Error> {
        match self {
            ObjEncoding::Weighted(enc, offset) => {
                if ub >= *offset {
                    enc.enforce_ub(ub - *offset)
                } else {
                    Err(rustsat::encodings::Error::Unsat)
                }
            }
            ObjEncoding::Unweighted(enc, offset) => {
                if ub >= *offset {
                    enc.enforce_ub(ub - *offset)
                } else {
                    Err(rustsat::encodings::Error::Unsat)
                }
            }
            ObjEncoding::Constant => Ok(vec![]),
        }
    }

    /// Gets a coarse upper bound
    #[cfg(feature = "coarse-convergence")]
    pub fn coarse_ub(&self, ub: usize) -> usize {
        match self {
            ObjEncoding::Weighted(enc, offset) => {
                if ub >= *offset {
                    enc.coarse_ub(ub - *offset) + offset
                } else {
                    ub
                }
            }
            _ => ub,
        }
    }
}

#[cfg(feature = "sol-tightening")]
/// Data regarding an objective literal
pub(crate) struct ObjLitData {
    /// Objectives that the literal appears in
    pub objs: Vec<usize>,
}
