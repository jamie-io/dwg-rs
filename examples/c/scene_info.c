/*
 * SPDX-FileCopyrightText: 2026 Jamie Jahn
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * scene_info: open DWG files through the C interface and print what a
 * viewer gets. Build (Linux):
 *   cargo build --release -p dwg2gltf-capi
 *   cc -std=c99 -Iinclude examples/c/scene_info.c -Ltarget/release -ldwg2gltf -o scene_info
 *   LD_LIBRARY_PATH=target/release ./scene_info drawing.dwg
 */
#include <stdio.h>

#include "dwg2gltf.h"

static int show(const char *path) {
    DwgOptions options;
    DwgScene *scene = NULL;
    const DwgSceneData *s;
    const uint8_t *glb = NULL;
    size_t glb_len = 0;
    uint32_t i, submeshes = 0, vertices = 0;
    int32_t status;

    dwg_options_init(&options);
    status = dwg_scene_open_file(path, &options, &scene);
    if (status != DWG_OK) {
        fprintf(stderr, "%s: error %d: %s\n", path, (int)status, dwg_last_error());
        return 1;
    }
    s = dwg_scene_data(scene);
    for (i = 0; i < s->mesh_count; i++) {
        submeshes += s->meshes[i].submesh_count;
        vertices += s->meshes[i].vertex_count;
    }
    printf("%s\n", path);
    printf("  meshes: %u (%u submeshes, %u vertices)\n", s->mesh_count, submeshes, vertices);
    printf("  nodes: %u, instances: %u, materials: %u, textures: %u\n", s->node_count,
           s->instance_count, s->material_count, s->texture_count);
    printf("  triangles: %llu stored, %llu drawn; line segments: %llu; visible edges: %llu\n",
           (unsigned long long)s->triangles_stored, (unsigned long long)s->triangles_drawn,
           (unsigned long long)s->line_segments, (unsigned long long)s->edge_count);
    printf("  units: INSUNITS %d, %g m per unit, %s\n", (int)s->insunits, s->unit_scale,
           (s->flags & DWG_SCENE_Z_UP) ? "Z up" : "Y up");
    if (s->flags & DWG_SCENE_HAS_BOUNDS)
        printf("  bounds: (%.4f %.4f %.4f) .. (%.4f %.4f %.4f) m\n", s->bounds_min[0],
               s->bounds_min[1], s->bounds_min[2], s->bounds_max[0], s->bounds_max[1],
               s->bounds_max[2]);
    for (i = 0; i < s->mesh_count && i < 5; i++) {
        const DwgMesh *m = &s->meshes[i];
        printf("  mesh %u \"%s\": %u triangles, %u lines, %u edges, material %u%s\n", i,
               m->name, m->triangle_count, m->line_count, m->edge_count,
               m->submesh_count ? m->submeshes[0].material : 0, m->uvs ? ", textured" : "");
    }
    for (i = 0; i < s->node_count && i < 5; i++) {
        const DwgNode *n = &s->nodes[i];
        printf("  node %u \"%s\" parent %d mesh %d block \"%s\"\n", i, n->name, (int)n->parent,
               (int)n->mesh, n->block_name);
    }
    for (i = 0; i < s->material_count && i < 5; i++) {
        const DwgMaterial *m = &s->materials[i];
        printf("  material %u \"%s\": rgba %.3f %.3f %.3f %.3f, texture %d\n", i, m->name,
               m->base_color[0], m->base_color[1], m->base_color[2], m->base_color[3],
               (int)m->texture);
    }
    for (i = 0; i < s->warning_count; i++)
        printf("  warning: %s\n", s->warnings[i]);
    if (dwg_scene_to_glb(scene, &glb, &glb_len) == DWG_OK)
        printf("  glb: %lu bytes\n", (unsigned long)glb_len);
    else
        fprintf(stderr, "%s: glb: %s\n", path, dwg_last_error());
    dwg_scene_free(scene);
    return 0;
}

int main(int argc, char **argv) {
    int i, failed = 0;
    if (argc < 2) {
        fprintf(stderr, "usage: scene_info FILE.dwg...\n");
        return 2;
    }
    printf("dwg2gltf %s (ABI %u)\n", dwg_version(), dwg_abi_version());
    for (i = 1; i < argc; i++)
        failed |= show(argv[i]);
    return failed;
}
