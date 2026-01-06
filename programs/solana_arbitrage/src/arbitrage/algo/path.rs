use std::{collections::HashSet, fmt::Debug};

use super::types::Amount;
use crate::arbitrage::base::Edge;

#[derive(Clone)]
pub struct Path<'info> {
    coef: f64,
    edges_set: HashSet<&'info Edge>,
    pub edges: Vec<&'info Edge>,
}

impl<'info> PartialEq for Path<'info> {
    fn eq(&self, other: &Self) -> bool {
        return self.coef == other.coef;
    }
}

impl<'info> Eq for Path<'info> {}

impl<'info> PartialOrd for Path<'info> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        return self.coef.partial_cmp(&other.coef);
    }
}

impl<'info> Debug for Path<'info> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(format!("{} {:?}", self.coef, self.edges).as_str())
    }
}

impl<'info> Ord for Path<'info> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl<'info> Path<'info> {
    pub fn new(edge: &'info Edge) -> Self {
        let set: HashSet<&'info Edge> = HashSet::new();
        let mut vec: Vec<&'info Edge> = Vec::new();
        vec.push(edge);
        Path {
            coef: edge.get_price(),
            edges_set: set,
            edges: vec,
        }
    }

    fn has_visited(&self, edge: &'info Edge) -> bool {
        return self.edges_set.contains(&edge);
    }

    pub fn first_edge(&self) -> Option<&&'info Edge> {
        return self.edges.get(0);
    }

    pub fn last_edge(&self) -> Option<&&'info Edge> {
        return self.edges.get(self.edges.len() - 1);
    }

    pub fn is_valid(&self) -> bool {
        // For arbitrage, we need at least 3 edges to form a cycle
        if self.edges.len() < 2 {
            return false;
        }

        // Check if the path forms a cycle (starts and ends with same currency)
        match (self.first_edge(), self.last_edge()) {
            (Some(fe), Some(le)) => fe.left.mint_account.eq(&le.right.mint_account),
            _ => false,
        }
    }

    pub fn add_edge(&self, edge: &'info Edge) -> Option<Self> {
        if self.has_visited(edge) {
            return None;
        }
        let mut cloned_set = self.edges_set.clone();
        let mut cloned_vec = self.edges.clone();
        cloned_set.insert(edge);
        cloned_vec.push(edge);
        Some(Path {
            coef: self.coef * edge.get_price(),
            edges: cloned_vec,
            edges_set: cloned_set,
        })
    }

    pub fn get_coef(&self) -> f64 {
        return self.coef;
    }

    pub fn compute_full_amount(&self, input: Amount) -> Amount {
        let value = input;
        for _edge in &self.edges {
            // TODO: Implement amount computation with fees
            // value = edge.compute_amount(value);
        }
        return value;
    }
}
