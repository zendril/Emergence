//! Methods to use [`Commands`] to manipulate terrain.

use bevy::{
    ecs::system::{Command, SystemState},
    prelude::{
        BuildWorldChildren, Commands, DespawnRecursiveExt, Handle, PbrBundle, Query, Res, ResMut,
        Transform, Vec3, Visibility, World,
    },
    scene::Scene,
};

use crate::{
    asset_management::manifest::Id,
    construction::{
        ghosts::{GhostHandles, GhostKind, GhostTerrainBundle, TerrainPreviewBundle},
        terraform::TerraformingAction,
        zoning::Zoning,
    },
    graphics::InheritedMaterial,
    simulation::geometry::{Height, MapGeometry, TilePos},
    terrain::{terrain_assets::TerrainHandles, terrain_manifest::Terrain},
};

use super::TerrainBundle;

/// An extension trait for [`Commands`] for working with terrain.
pub(crate) trait TerrainCommandsExt {
    /// Spawns a new terrain tile.
    ///
    /// Overwrites existing terrain.
    fn spawn_terrain(&mut self, tile_pos: TilePos, height: Height, terrain_id: Id<Terrain>);

    /// Spawns a ghost that previews the action given by `terraforming_action` at `tile_pos`.
    ///
    /// Replaces any existing ghost.
    fn spawn_ghost_terrain(
        &mut self,
        tile_pos: TilePos,
        terrain_id: Id<Terrain>,
        terraforming_action: TerraformingAction,
    );

    /// Despawns any ghost at the provided `tile_pos`.
    ///
    /// Has no effect if the tile position is already empty.
    fn despawn_ghost_terrain(&mut self, tile_pos: TilePos);

    /// Spawns a preview that previews the action given by `terraforming_action` at `tile_pos`.
    fn spawn_preview_terrain(
        &mut self,
        tile_pos: TilePos,
        terrain_id: Id<Terrain>,
        terraforming_action: TerraformingAction,
    );

    /// Applies the given `terraforming_action` to the terrain at `tile_pos`.
    fn apply_terraforming_action(&mut self, tile_pos: TilePos, action: TerraformingAction);
}

impl<'w, 's> TerrainCommandsExt for Commands<'w, 's> {
    fn spawn_terrain(&mut self, tile_pos: TilePos, height: Height, terrain_id: Id<Terrain>) {
        self.add(SpawnTerrainCommand {
            tile_pos,
            height,
            terrain_id,
        });
    }

    fn spawn_ghost_terrain(
        &mut self,
        tile_pos: TilePos,
        terrain_id: Id<Terrain>,
        terraforming_action: TerraformingAction,
    ) {
        self.add(SpawnTerrainGhostCommand {
            tile_pos,
            terrain_id,
            terraforming_action,
            ghost_kind: GhostKind::Ghost,
        });
    }

    fn despawn_ghost_terrain(&mut self, tile_pos: TilePos) {
        self.add(DespawnGhostCommand { tile_pos });
    }

    fn spawn_preview_terrain(
        &mut self,
        tile_pos: TilePos,
        terrain_id: Id<Terrain>,
        terraforming_action: TerraformingAction,
    ) {
        self.add(SpawnTerrainGhostCommand {
            tile_pos,
            terrain_id,
            terraforming_action,
            ghost_kind: GhostKind::Preview,
        });
    }

    fn apply_terraforming_action(
        &mut self,
        tile_pos: TilePos,
        terraforming_action: TerraformingAction,
    ) {
        self.add(ApplyTerraformingCommand {
            tile_pos,
            terraforming_action,
        });
    }
}

/// Constructs a new [`Terrain`] entity.
///
/// The order of the chidlren *must* be:
/// 0: column
/// 1: overlay
/// 2: scene root
pub(crate) struct SpawnTerrainCommand {
    /// The position to spawn the tile
    pub(crate) tile_pos: TilePos,
    /// The height of the tile
    pub(crate) height: Height,
    /// The type of tile
    pub(crate) terrain_id: Id<Terrain>,
}

impl Command for SpawnTerrainCommand {
    fn write(self, world: &mut World) {
        let handles = world.resource::<TerrainHandles>();
        let scene_handle = handles.scenes.get(&self.terrain_id).unwrap().clone_weak();
        let mesh = handles.topper_mesh.clone_weak();
        let mut map_geometry = world.resource_mut::<MapGeometry>();

        // Store the height, so it can be used below
        map_geometry.update_height(self.tile_pos, self.height);

        // Drop the borrow so the borrow checker is happy
        let map_geometry = world.resource::<MapGeometry>();

        // Spawn the terrain entity
        let terrain_entity = world
            .spawn(TerrainBundle::new(
                self.terrain_id,
                self.tile_pos,
                scene_handle,
                mesh,
                map_geometry,
            ))
            .id();

        // Spawn the column as the 0th child of the tile entity
        // The scene bundle will be added as the first child
        let handles = world.resource::<TerrainHandles>();
        let column_bundle = PbrBundle {
            mesh: handles.column_mesh.clone_weak(),
            material: handles.column_material.clone_weak(),
            ..Default::default()
        };

        let hex_column = world.spawn(column_bundle).id();
        world.entity_mut(terrain_entity).add_child(hex_column);

        let handles = world.resource::<TerrainHandles>();
        /// Makes the overlays ever so slightly larger than their base to avoid z-fighting.
        ///
        /// This value should be very slightly larger than 1.0
        const OVERLAY_OVERSIZE_SCALE: f32 = 1.001;

        let overlay_bundle = PbrBundle {
            mesh: handles.topper_mesh.clone_weak(),
            visibility: Visibility::Hidden,
            transform: Transform::from_scale(Vec3 {
                x: OVERLAY_OVERSIZE_SCALE,
                y: OVERLAY_OVERSIZE_SCALE,
                z: OVERLAY_OVERSIZE_SCALE,
            }),
            ..Default::default()
        };
        let overlay = world.spawn(overlay_bundle).id();
        world.entity_mut(terrain_entity).add_child(overlay);

        // Update the index of what terrain is where
        let mut map_geometry = world.resource_mut::<MapGeometry>();
        map_geometry.add_terrain(self.tile_pos, terrain_entity);
    }
}

/// A [`Command`] used to spawn a ghost via [`TerrainCommandsExt`].
struct SpawnTerrainGhostCommand {
    /// The tile position at which the ghost should be spawned.
    tile_pos: TilePos,
    /// The terrain type that the ghost represents.
    terrain_id: Id<Terrain>,
    /// The action that the ghost represents.
    terraforming_action: TerraformingAction,
    /// What kind of ghost this is.
    ghost_kind: GhostKind,
}

impl Command for SpawnTerrainGhostCommand {
    fn write(self, world: &mut World) {
        let map_geometry = world.resource::<MapGeometry>();

        // Check that the tile is within the bounds of the map
        if !map_geometry.is_valid(self.tile_pos) {
            return;
        }

        // Remove any existing ghost terrain
        if let Some(ghost_entity) = map_geometry.get_ghost_terrain(self.tile_pos) {
            if world.entities().contains(ghost_entity) && self.ghost_kind == GhostKind::Ghost {
                world.entity_mut(ghost_entity).despawn_recursive();
                let mut map_geometry = world.resource_mut::<MapGeometry>();
                map_geometry.remove_ghost_terrain(self.tile_pos);
            }
        }

        let map_geometry = world.resource::<MapGeometry>();
        let scene_handle = world
            .resource::<TerrainHandles>()
            .scenes
            .get(&self.terrain_id)
            .unwrap()
            .clone_weak();

        let ghost_handles = world.resource::<GhostHandles>();
        let ghost_material = ghost_handles.get_material(self.ghost_kind);

        let inherited_material = InheritedMaterial(ghost_material);
        let current_height = map_geometry.get_height(self.tile_pos).unwrap();
        let new_height = match self.terraforming_action {
            TerraformingAction::Raise => current_height + Height(1.),
            TerraformingAction::Lower => current_height - Height(1.),
            _ => current_height,
        };

        let mut world_pos = self.tile_pos.into_world_pos(map_geometry);
        world_pos.y = new_height.into_world_pos();

        match self.ghost_kind {
            GhostKind::Ghost => {
                let input_inventory = self.terraforming_action.input_inventory();
                let output_inventory = self.terraforming_action.output_inventory();

                let ghost_entity = world
                    .spawn(GhostTerrainBundle::new(
                        self.terraforming_action,
                        self.tile_pos,
                        scene_handle,
                        inherited_material,
                        world_pos,
                        input_inventory,
                        output_inventory,
                    ))
                    .id();

                // Update the index to reflect the new state
                let mut map_geometry = world.resource_mut::<MapGeometry>();
                map_geometry.add_ghost_terrain(ghost_entity, self.tile_pos);
            }
            GhostKind::Preview => {
                // Previews are not indexed, and are instead just spawned and despawned as needed
                world.spawn(TerrainPreviewBundle::new(
                    self.tile_pos,
                    self.terraforming_action,
                    scene_handle,
                    inherited_material,
                    world_pos,
                ));
            }
            _ => unreachable!("Invalid ghost kind provided."),
        }
    }
}

/// A [`Command`] used to despawn a ghost via [`TerrainCommandsExt`].
struct DespawnGhostCommand {
    /// The tile position at which the terrain to be despawned is found.
    tile_pos: TilePos,
}

impl Command for DespawnGhostCommand {
    fn write(self, world: &mut World) {
        let mut geometry = world.resource_mut::<MapGeometry>();
        let maybe_entity = geometry.remove_ghost_terrain(self.tile_pos);

        // Check that there's something there to despawn
        let Some(ghost_entity) = maybe_entity else {
            return;
        };

        // Make sure to despawn all children, which represent the meshes stored in the loaded gltf scene.
        world.entity_mut(ghost_entity).despawn_recursive();
    }
}

/// A [`Command`] used to apply [`TerraformingAction`]s to a tile.
struct ApplyTerraformingCommand {
    /// The tile position at which the terrain to be despawned is found.
    tile_pos: TilePos,
    /// The action to apply to the tile.
    terraforming_action: TerraformingAction,
}

impl Command for ApplyTerraformingCommand {
    fn write(self, world: &mut World) {
        // Just using system state makes satisfying the borrow checker a lot easier
        let mut system_state = SystemState::<(
            ResMut<MapGeometry>,
            Res<TerrainHandles>,
            Query<(
                &mut Id<Terrain>,
                &mut Zoning,
                &mut Height,
                &mut Handle<Scene>,
            )>,
        )>::new(world);

        let (mut map_geometry, terrain_handles, mut terrain_query) = system_state.get_mut(world);

        let terrain_entity = map_geometry.get_terrain(self.tile_pos).unwrap();

        let (mut current_terrain_id, mut zoning, mut height, mut scene_handle) =
            terrain_query.get_mut(terrain_entity).unwrap();

        match self.terraforming_action {
            TerraformingAction::Raise => height.raise(),
            TerraformingAction::Lower => height.lower(),
            TerraformingAction::Change(changed_terrain_id) => {
                *current_terrain_id = changed_terrain_id;
            }
        };

        // We can't do this above, as we need to drop the previous query before borrowing from the world again
        if let TerraformingAction::Change(changed_terrain_id) = self.terraforming_action {
            *scene_handle = terrain_handles
                .scenes
                .get(&changed_terrain_id)
                .unwrap()
                .clone_weak();
        }

        map_geometry.update_height(self.tile_pos, *height);
        *zoning = Zoning::None;
    }
}
