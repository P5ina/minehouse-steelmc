//! Asynchronous vanilla `ChunkGenerator.findNearestMapStructure`.
//!
//! Vanilla generates the candidate chunks up to `STRUCTURE_STARTS` while it
//! searches. Steel requests those chunks and is polled until they are ready,
//! so `/locate` and mobs such as the dolphin share one search.

use std::sync::Arc;

use steel_utils::{BlockPos, Identifier};

use crate::chunk::chunk_request::{
    ChunkRequest, ChunkRequestHandle, ChunkRequestState, ChunkTicketKind,
};
use crate::chunk::status::ChunkStatus;
use crate::world::World;
use crate::worldgen::generator::ChunkGenerator;
use crate::worldgen::structure::{StructureLocateCandidate, StructureLocatePlan, squared_distance};

/// The structure a search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundStructure {
    /// Vanilla `getLocatePos` of the structure start.
    pub pos: BlockPos,
    /// The structure that was found there.
    pub structure: Identifier,
}

/// The state of a [`StructureSearch`] after a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureSearchPoll {
    /// Candidate chunks are still generating.
    Pending,
    /// The nearest structure within the radius.
    Found(FoundStructure),
    /// No structure within the radius.
    NotFound,
    /// The chunk request was cancelled.
    Cancelled,
}

enum SearchPhase {
    Start,
    WaitingRings,
    RandomSpread,
    WaitingRandomSpread,
}

struct LocatedCandidate {
    candidate: StructureLocateCandidate,
    found_structure: Identifier,
    distance_sqr: i64,
}

/// A nearest-structure search over concentric-ring and random-spread
/// placements, polled until it finishes.
pub struct StructureSearch {
    world: Arc<World>,
    plan: StructureLocatePlan,
    origin: BlockPos,
    max_radius: i32,
    phase: SearchPhase,
    pending: Option<ChunkRequestHandle>,
    candidates: Vec<StructureLocateCandidate>,
    best: Option<LocatedCandidate>,
    random_radius: i32,
}

impl StructureSearch {
    /// Starts a search for any of `structures` around `origin`, scanning
    /// random-spread placements up to `max_radius` chunks out. Returns `None`
    /// when the world generator places none of them.
    #[must_use]
    pub fn new(
        world: Arc<World>,
        structures: &[Identifier],
        origin: BlockPos,
        max_radius: i32,
    ) -> Option<Self> {
        let plan = world
            .chunk_map
            .world_gen_context
            .generator
            .structure_generator()?
            .locate_plan_for_structures(structures)?;
        if plan.is_empty() {
            return None;
        }

        Some(Self {
            world,
            plan,
            origin,
            max_radius,
            phase: SearchPhase::Start,
            pending: None,
            candidates: Vec::new(),
            best: None,
            random_radius: 0,
        })
    }

    /// Advances the search as far as generated chunks allow.
    pub fn poll(&mut self) -> StructureSearchPoll {
        loop {
            match self.phase {
                SearchPhase::Start => {
                    self.candidates = self.plan.ring_candidates(self.origin);
                    if self.candidates.is_empty() {
                        self.phase = SearchPhase::RandomSpread;
                        continue;
                    }
                    self.pending = Some(self.request_current_candidates());
                    self.phase = SearchPhase::WaitingRings;
                    return StructureSearchPoll::Pending;
                }
                SearchPhase::WaitingRings => match self.poll_pending_request() {
                    PendingRequest::Pending => return StructureSearchPoll::Pending,
                    PendingRequest::Cancelled => return StructureSearchPoll::Cancelled,
                    PendingRequest::Ready => {
                        self.best = self.first_valid_candidate();
                        self.clear_request();

                        if self.best.is_some() && !self.plan.has_random_spread() {
                            return self.finished();
                        }

                        self.phase = SearchPhase::RandomSpread;
                    }
                },
                SearchPhase::RandomSpread => {
                    if self.random_radius > self.max_radius {
                        return self.finished();
                    }

                    self.candidates = self
                        .plan
                        .random_spread_candidates_at_radius(self.origin, self.random_radius);
                    self.random_radius += 1;

                    if self.candidates.is_empty() {
                        continue;
                    }

                    self.pending = Some(self.request_current_candidates());
                    self.phase = SearchPhase::WaitingRandomSpread;
                    return StructureSearchPoll::Pending;
                }
                SearchPhase::WaitingRandomSpread => match self.poll_pending_request() {
                    PendingRequest::Pending => return StructureSearchPoll::Pending,
                    PendingRequest::Cancelled => return StructureSearchPoll::Cancelled,
                    PendingRequest::Ready => {
                        if self.update_best_after_random_radius() {
                            return self.finished();
                        }

                        self.clear_request();
                        self.phase = SearchPhase::RandomSpread;
                    }
                },
            }
        }
    }

    /// Cancels the pending chunk request, if any.
    pub fn cancel(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.cancel();
        }
    }

    fn finished(&self) -> StructureSearchPoll {
        self.best
            .as_ref()
            .map_or(StructureSearchPoll::NotFound, |best| {
                StructureSearchPoll::Found(FoundStructure {
                    pos: best.candidate.locate_pos,
                    structure: best.found_structure.clone(),
                })
            })
    }

    fn request_current_candidates(&self) -> ChunkRequestHandle {
        let positions = self
            .candidates
            .iter()
            .map(|candidate| candidate.chunk_pos)
            .collect();
        self.world.chunk_map.request_chunks(ChunkRequest {
            status: ChunkStatus::StructureStarts,
            positions,
            ticket_kind: ChunkTicketKind::StructureLocate,
        })
    }

    fn poll_pending_request(&self) -> PendingRequest {
        let Some(pending) = &self.pending else {
            return PendingRequest::Cancelled;
        };

        match pending.poll() {
            ChunkRequestState::Pending { .. } => PendingRequest::Pending,
            ChunkRequestState::Ready => PendingRequest::Ready,
            ChunkRequestState::Cancelled => PendingRequest::Cancelled,
        }
    }

    fn clear_request(&mut self) {
        self.pending = None;
        self.candidates.clear();
    }

    fn first_valid_candidate(&self) -> Option<LocatedCandidate> {
        self.candidates.iter().copied().find_map(|candidate| {
            self.generated_structure_at_candidate(candidate)
                .map(|found_structure| LocatedCandidate {
                    candidate,
                    found_structure,
                    distance_sqr: squared_distance(candidate.locate_pos, self.origin),
                })
        })
    }

    fn update_best_after_random_radius(&mut self) -> bool {
        let mut best = self.best.take();
        let mut current_scan = None;
        let mut found_current_scan = false;
        let mut found_in_this_radius = false;

        for candidate in &self.candidates {
            if current_scan != Some(candidate.scan_id()) {
                current_scan = Some(candidate.scan_id());
                found_current_scan = false;
            }

            if found_current_scan {
                continue;
            }

            let Some(found_structure) = self.generated_structure_at_candidate(*candidate) else {
                continue;
            };
            found_current_scan = true;
            found_in_this_radius = true;
            let located = LocatedCandidate {
                candidate: *candidate,
                found_structure,
                distance_sqr: squared_distance(candidate.locate_pos, self.origin),
            };
            if best
                .as_ref()
                .is_none_or(|current| located.distance_sqr < current.distance_sqr)
            {
                best = Some(located);
            }
        }

        self.best = best;
        found_in_this_radius
    }

    fn generated_structure_at_candidate(
        &self,
        candidate: StructureLocateCandidate,
    ) -> Option<Identifier> {
        let holder = self
            .world
            .chunk_map
            .chunks
            .read_sync(&candidate.chunk_pos, |_, holder| Arc::clone(holder))?;
        let chunk = holder.try_chunk(ChunkStatus::StructureStarts)?;
        let starts = chunk.structure_starts();
        let structures = self.plan.structures_for_candidate(candidate)?;
        structures.iter().find_map(|structure| {
            starts
                .get(structure)
                .is_some_and(|start| !start.pieces.is_empty())
                .then(|| structure.clone())
        })
    }
}

enum PendingRequest {
    Pending,
    Ready,
    Cancelled,
}
