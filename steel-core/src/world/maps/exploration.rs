//! Vanilla exploration maps: the structure search behind `ExplorationMapFunction`.
//!
//! Vanilla searches on the server thread and freezes it while candidate chunks
//! generate. Steel cannot drive chunk generation inside a tick, so the search runs
//! across ticks: a loot roll that needs one waits for it, then rolls again with the
//! same seed and the answers in hand.

use std::mem;
use std::sync::Arc;

use steel_registry::data_components::components::{MapDecorations, MapId};
use steel_registry::data_components::vanilla_components::{MAP_DECORATIONS, MAP_ID};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::{ExplorationMapRequest, ExplorationMaps};
use steel_registry::{REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _};
use steel_utils::{BlockPos, ChunkPos, Identifier, SectionPos};
use uuid::Uuid;

use crate::behavior::BLOCK_BEHAVIORS;
use crate::behavior::items::{
    add_target_decoration, apply_new_saved_data, render_biome_preview_map,
};
use crate::block_entity::container_loot::{ContainerLoot, LootUnpack};
use crate::chunk::status::ChunkStatus;
use crate::inventory::container::{Container as _, SimpleContainer};
use crate::world::structure_search::{StructureSearch, StructureSearchPoll};
use crate::world::{LevelReader as _, World};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;

/// Vanilla `StructureStart.getMaxReferences`.
const MAX_REFERENCES: i32 = 1;

/// One structure search for an exploration map, polled until it finishes.
pub struct ExplorationMapSearch {
    search: Option<StructureSearch>,
}

impl ExplorationMapSearch {
    /// Vanilla `ExplorationMapFunction.run` up to `findNearestMapStructure`: claims
    /// the structure the loot is in, then starts looking for the nearest other one.
    #[must_use]
    pub fn start(world: &Arc<World>, request: &ExplorationMapRequest) -> Self {
        let (x, y, z) = request.origin;
        let origin = BlockPos::containing(x, y, z);
        let structures: Vec<Identifier> = REGISTRY
            .structures
            .iter_tag(&request.destination)
            .map(|structure| structure.key.clone())
            .collect();
        if request.skip_existing_chunks {
            world.claim_structure_at(&structures, origin);
        }
        Self {
            search: StructureSearch::new(
                Arc::clone(world),
                &structures,
                origin,
                request.search_radius,
                request.skip_existing_chunks,
            ),
        }
    }

    /// The answer once the search is done, `None` while it runs.
    pub fn poll(&mut self) -> Option<ExplorationMapAnswer> {
        let Some(search) = &mut self.search else {
            return Some(ExplorationMapAnswer::NotFound);
        };
        match search.poll() {
            StructureSearchPoll::Pending => None,
            StructureSearchPoll::Found(found) => Some(ExplorationMapAnswer::Found {
                pos: found.pos,
                map: None,
            }),
            StructureSearchPoll::NotFound | StructureSearchPoll::Cancelled => {
                Some(ExplorationMapAnswer::NotFound)
            }
        }
    }
}

/// The outcome of one exploration map roll.
#[derive(Debug, Clone)]
pub enum ExplorationMapAnswer {
    /// No structure in reach: vanilla leaves the item as it is.
    NotFound,
    /// The structure the map leads to, and the map once it was drawn.
    Found {
        /// Vanilla `getLocatePos` of the structure.
        pos: BlockPos,
        /// The new map's id and decorations, kept so a later roll of the same loot
        /// reuses them instead of drawing the map again.
        map: Option<(MapId, MapDecorations)>,
    },
}

/// Answers the exploration map rolls of one loot roll, in roll order, from
/// searches that already finished.
pub struct ExplorationMapAnswers<'a> {
    world: &'a World,
    answers: &'a mut Vec<ExplorationMapAnswer>,
    next: usize,
    missing: Option<ExplorationMapRequest>,
}

impl<'a> ExplorationMapAnswers<'a> {
    /// Answers from `answers`; the first roll past them is kept as missing.
    pub const fn new(world: &'a World, answers: &'a mut Vec<ExplorationMapAnswer>) -> Self {
        Self {
            world,
            answers,
            next: 0,
            missing: None,
        }
    }

    /// The first roll that still needs a search.
    #[must_use]
    pub fn into_missing(self) -> Option<ExplorationMapRequest> {
        self.missing
    }
}

impl ExplorationMaps for ExplorationMapAnswers<'_> {
    /// Vanilla `ExplorationMapFunction.run` after the search.
    fn create_exploration_map(&mut self, item: &mut ItemStack, request: ExplorationMapRequest) {
        let index = self.next;
        self.next += 1;
        let Some(answer) = self.answers.get_mut(index) else {
            if self.missing.is_none() {
                self.missing = Some(request);
            }
            return;
        };
        let ExplorationMapAnswer::Found { pos, map } = answer else {
            return;
        };
        if let Some((id, decorations)) = map {
            item.set(MAP_ID, *id);
            item.set(MAP_DECORATIONS, decorations.clone());
            return;
        }
        let Some(decoration) = REGISTRY.map_decoration_types.by_key(&request.decoration) else {
            log::warn!("Unknown map decoration type {}", request.decoration);
            return;
        };
        let zoom = u8::try_from(request.zoom.clamp(0, 4)).unwrap_or_default();
        apply_new_saved_data(self.world, item, pos.x(), pos.z(), zoom, true, true);
        render_biome_preview_map(self.world, item);
        add_target_decoration(item, *pos, "+", RegistryReference::new(decoration));
        if let (Some(id), Some(decorations)) = (item.get(MAP_ID), item.get(MAP_DECORATIONS)) {
            *map = Some((*id, decorations.clone()));
        }
    }
}

/// What to do with a container's loot once its exploration map searches end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LootWaiter {
    /// Open the container for the player who tried to.
    Reopen { player: Uuid, pos: BlockPos },
    /// The container broke while waiting: drop the loot where it stood.
    DropAt(BlockPos),
}

struct WaitingLoot {
    /// Kept alive here: a broken container's loot has no other owner.
    loot: Arc<ContainerLoot>,
    waiters: Vec<LootWaiter>,
}

/// Container loot waiting on exploration map searches, polled every tick.
#[derive(Default)]
pub struct ExplorationLoot {
    waiting: Vec<WaitingLoot>,
}

impl ExplorationLoot {
    fn entry(&mut self, loot: &Arc<ContainerLoot>) -> &mut WaitingLoot {
        let index = self
            .waiting
            .iter()
            .position(|waiting| Arc::ptr_eq(&waiting.loot, loot))
            .unwrap_or_else(|| {
                self.waiting.push(WaitingLoot {
                    loot: Arc::clone(loot),
                    waiters: Vec::new(),
                });
                self.waiting.len() - 1
            });
        &mut self.waiting[index]
    }
}

impl World {
    /// Vanilla `ExplorationMapFunction.claimStructureAt`: the structure the loot
    /// sits in takes a reference, so its own maps lead elsewhere.
    pub(crate) fn claim_structure_at(&self, structures: &[Identifier], pos: BlockPos) {
        let chunk_pos = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        let sources: Vec<(Identifier, ChunkPos)> = self
            .chunk_map
            .with_chunk_at_status(chunk_pos, ChunkStatus::StructureReferences, |chunk| {
                let references = chunk.structure_references();
                structures
                    .iter()
                    .filter_map(|structure| {
                        references
                            .get(structure)
                            .map(|sources| (structure, sources))
                    })
                    .flat_map(|(structure, sources)| {
                        sources.iter().map(|source| (structure.clone(), *source))
                    })
                    .collect()
            })
            .unwrap_or_default();

        for (structure, source) in sources {
            let claimed = self
                .chunk_map
                .with_chunk_at_status(source, ChunkStatus::StructureStarts, |chunk| {
                    let mut starts = chunk.structure_starts_mut();
                    let start = starts.get_mut(&structure)?;
                    if !start
                        .bounding_box
                        .is_some_and(|bounding_box| bounding_box.contains_blockpos(pos))
                    {
                        return None;
                    }
                    if !start.pieces.is_empty() && start.references < MAX_REFERENCES {
                        start.references += 1;
                        chunk.mark_dirty();
                    }
                    Some(())
                })
                .flatten();
            if claimed.is_some() {
                return;
            }
        }
    }

    /// Keeps polling `loot`'s exploration map searches, acting for `waiters` once
    /// they end.
    fn wait_for_exploration_loot(
        &self,
        loot: &Arc<ContainerLoot>,
        waiters: impl IntoIterator<Item = LootWaiter>,
    ) {
        let mut waiting = self.exploration_loot.lock();
        let entry = waiting.entry(loot);
        for waiter in waiters {
            if !entry.waiters.contains(&waiter) {
                entry.waiters.push(waiter);
            }
        }
    }

    /// The player tried to open a container whose loot still waits on a search;
    /// the container opens for them once it is rolled.
    pub(crate) fn reopen_after_exploration_loot(
        &self,
        loot: &Arc<ContainerLoot>,
        player: Uuid,
        pos: BlockPos,
    ) {
        self.wait_for_exploration_loot(loot, [LootWaiter::Reopen { player, pos }]);
    }

    /// The container broke while its loot waited on a search; the loot drops at
    /// `pos` once it is rolled.
    pub(crate) fn drop_after_exploration_loot(&self, loot: &Arc<ContainerLoot>, pos: BlockPos) {
        self.wait_for_exploration_loot(loot, [LootWaiter::DropAt(pos)]);
    }

    /// Starts polling `loot`'s searches every tick.
    pub(crate) fn track_exploration_loot(&self, loot: &Arc<ContainerLoot>) {
        self.wait_for_exploration_loot(loot, []);
    }

    /// Polls the exploration map searches of waiting container loot; once one
    /// answers, whoever waits rolls the loot again, which may start the next search.
    pub(crate) fn tick_exploration_loot(self: &Arc<Self>) {
        let waiting = mem::take(&mut self.exploration_loot.lock().waiting);
        for entry in waiting {
            let loot = entry.loot;
            if loot.advance_exploration_search() {
                self.wait_for_exploration_loot(&loot, entry.waiters);
                continue;
            }
            for waiter in entry.waiters {
                match waiter {
                    LootWaiter::Reopen { player, pos } => self.reopen_container(player, pos),
                    LootWaiter::DropAt(pos) => {
                        if !self.drop_exploration_loot(&loot, pos) {
                            self.wait_for_exploration_loot(&loot, [waiter]);
                        }
                    }
                }
            }
        }
    }

    fn reopen_container(self: &Arc<Self>, player: Uuid, pos: BlockPos) {
        let Some(player) = self.players.get_by_uuid(&player) else {
            return;
        };
        let state = self.get_block_state(pos);
        BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .open_menu_provider(state, self, pos, &player);
    }

    /// Rolls loot whose container is gone and drops it; false while it still waits.
    fn drop_exploration_loot(self: &Arc<Self>, loot: &Arc<ContainerLoot>, pos: BlockPos) -> bool {
        let mut container = SimpleContainer::new(27);
        if loot.unpack(&mut container, pos, None, self) == LootUnpack::Searching {
            return false;
        }
        for item in container.items_mut().iter_mut().map(mem::take) {
            self.drop_item_stack(pos, item);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::data_components::vanilla_components::{MAP_DECORATIONS, MAP_ID};
    use steel_registry::item_stack::ItemStack;
    use steel_registry::loot_table::{ExplorationMapRequest, ExplorationMaps as _};
    use steel_registry::{init_vanilla_registry, vanilla_items};
    use steel_utils::{BlockPos, Identifier};

    use super::{ExplorationMapAnswer, ExplorationMapAnswers};
    use crate::behavior::init_behaviors;
    use crate::test_support::fresh_test_world;

    fn treasure_request() -> ExplorationMapRequest {
        ExplorationMapRequest {
            destination: Identifier::vanilla_static("on_treasure_maps"),
            decoration: Identifier::vanilla_static("red_x"),
            zoom: 1,
            search_radius: 50,
            skip_existing_chunks: false,
            origin: (0.5, 64.5, 0.5),
        }
    }

    #[test]
    fn a_found_structure_draws_one_map_that_later_rolls_reuse() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("exploration_map_answers");
        let world = &fixture.world;
        let target = BlockPos::new(1000, 64, -300);
        let mut answers = vec![ExplorationMapAnswer::Found {
            pos: target,
            map: None,
        }];

        let mut first = ItemStack::new(&vanilla_items::FILLED_MAP);
        let mut maps = ExplorationMapAnswers::new(world, &mut answers);
        maps.create_exploration_map(&mut first, treasure_request());
        // A third roll past the answers waits for its own search.
        let mut unanswered = ItemStack::new(&vanilla_items::FILLED_MAP);
        maps.create_exploration_map(&mut unanswered, treasure_request());
        assert_eq!(maps.into_missing(), Some(treasure_request()));
        assert!(!unanswered.has(MAP_ID));

        let id = *first.get(MAP_ID).expect("the map got saved data");
        {
            let data = world.map_data(id).expect("map data");
            let data = data.lock();
            assert_eq!(data.scale, 1);
            assert_eq!((data.center_x, data.center_z), (1088, -192));
        }
        let decorations = first.get(MAP_DECORATIONS).expect("the target marker");
        let target_marker = decorations.decorations().get("+").expect("keyed +");
        assert_eq!(
            (
                target_marker.x(),
                target_marker.z(),
                target_marker.rotation()
            ),
            (1000.0, -300.0, 180.0)
        );

        // Replaying the roll hands out the same map instead of drawing a new one.
        let mut replayed = ItemStack::new(&vanilla_items::FILLED_MAP);
        ExplorationMapAnswers::new(world, &mut answers)
            .create_exploration_map(&mut replayed, treasure_request());
        assert_eq!(replayed.get(MAP_ID), Some(&id));
        assert!(ItemStack::is_same_item_same_components(&replayed, &first));

        // No structure: the item is left as it was.
        let mut answers = vec![ExplorationMapAnswer::NotFound];
        let mut plain = ItemStack::new(&vanilla_items::FILLED_MAP);
        ExplorationMapAnswers::new(world, &mut answers)
            .create_exploration_map(&mut plain, treasure_request());
        assert!(!plain.has(MAP_ID));
    }
}
