/*
 * SPDX-FileCopyrightText: 2026 Jamie Jahn
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * dwg2gltf.h - C interface of dwg2gltf.dll / libdwg2gltf.so (C99).
 *
 * Converts a DWG drawing into a scene a viewer can upload directly: meshes
 * (positions, normals, UVs, indices, submeshes per material), the block
 * instance tree with matrices, materials, textures (encoded image files),
 * visible edges per mesh, bounds, units, warnings and statistics. The same
 * scene is available as a GLB file.
 *
 * Coordinates
 *   The scene is in glTF's frame: right-handed, Y up, meters. Mesh positions
 *   are in their block's own coordinates (DWG units, DWG axes); a node's
 *   world matrix maps them into the scene, the unit scale and the Z-up to
 *   Y-up turn included (both are in the root node's matrix). With
 *   DWG_OPT_Z_UP the scene keeps DWG's Z-up axes (still in meters).
 *   For a left-handed renderer (Direct3D's usual setup) multiply the view by
 *   a mirror in Z (scale 1, 1, -1) and treat clockwise triangles as front
 *   faces, or simply draw without culling (every triangle material is
 *   double-sided).
 *
 * Matrices
 *   16 values, column-major as in glTF and OpenGL: m[c * 4 + r] is row r of
 *   column c of M, with p' = M * p for column vectors p = (x, y, z, 1); the
 *   translation is m[12], m[13], m[14]. The same 16 values in the same order
 *   are the row-vector matrix (p' = p * M) of Direct3D / DirectXMath, so
 *   XMLoadFloat4x4 takes DwgInstance.world as it is.
 *   DwgNode holds doubles: far from the origin float rounds a translation by
 *   more than a millimeter; subtract a reference point in double before
 *   converting to float if the drawing is that large.
 *
 * Colors
 *   DwgMaterial.base_color is linear RGB (glTF's baseColorFactor, converted
 *   from the drawing's sRGB colors), alpha = opacity. Render into an sRGB
 *   target, or convert with the sRGB transfer function. Base color textures
 *   are sRGB-encoded images, normal maps are linear.
 *
 * Visible edges
 *   Per mesh, a line list into its positions: boundary edges (one
 *   triangle), non-manifold edges (three or more), and creases, where two
 *   triangles fold by more than crease_angle and the mesh's vertex normals
 *   are not continuous across the edge (so smooth-shaded curved surfaces
 *   show no facet lines). Vertices are welded by position first; coplanar
 *   splits and seams give no edges.
 *
 * Lifetime and threads
 *   Every pointer reached through a scene stays valid until dwg_scene_free.
 *   Strings are UTF-8 and NUL-terminated. A scene may be read from several
 *   threads at once. Opening scenes from several threads is safe, but
 *   decoding is serialized (LibreDWG keeps global state). Conversion runs on
 *   a thread of its own with a large stack.
 *   Errors in the converter are caught and returned as codes; a crash inside
 *   LibreDWG's C code on a corrupt file cannot be caught and ends the
 *   process. Run untrusted files in a separate process.
 */
#ifndef DWG2GLTF_H
#define DWG2GLTF_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32) && !defined(DWG2GLTF_STATIC)
#define DWG_API __declspec(dllimport)
#else
#define DWG_API
#endif

/* Bumped when a struct or function changes incompatibly. Structs only grow
 * at their end; their `size` fields say how much the other side knows. */
#define DWG_ABI 1

/* Status codes. */
#define DWG_OK 0
#define DWG_ERR_ARGUMENT 1       /* a NULL pointer, or invalid options */
#define DWG_ERR_IO 2             /* the file could not be read */
#define DWG_ERR_READ 3           /* LibreDWG could not decode the drawing */
#define DWG_ERR_NO_MODEL_SPACE 4 /* the drawing has no model space block */
#define DWG_ERR_EMPTY 5          /* nothing to show (DWG_OPT_EMPTY_IS_ERROR) */
#define DWG_ERR_NON_FINITE 6     /* a non-finite number reached the output */
#define DWG_ERR_INTERNAL 7       /* anything else, also a converter panic */

/* DwgOptions.flags; all clear is the default. */
#define DWG_OPT_NO_LINES 1       /* leave out wire geometry (lines, arcs, polylines) */
#define DWG_OPT_PLAN_SYMBOLS 2   /* keep OFML 2D plan symbols (left out by default) */
#define DWG_OPT_NO_EDGES 4       /* do not compute visible edges */
#define DWG_OPT_Z_UP 8           /* keep DWG's Z-up axes */
#define DWG_OPT_EMPTY_IS_ERROR 16 /* a drawing with nothing to show is DWG_ERR_EMPTY
                                     (default: an empty scene with a warning) */

/* DwgOptions.units: 0 = the drawing's units (INSUNITS; none = meters),
 * 1..24 = that INSUNITS code instead (4 mm, 5 cm, 6 m, 1 inch, 2 feet ...),
 * DWG_UNITS_AUTO = the drawing's units, but meters when the drawing would be
 * less than 1 cm across (OFML geometry stored in meters in a mm drawing). */
#define DWG_UNITS_DRAWING 0
#define DWG_UNITS_AUTO (-1)

/* DwgSubmesh.topology */
#define DWG_TRIANGLES 0 /* triangle list */
#define DWG_LINES 1     /* line list (wire geometry; normals and UVs are 0) */

/* DwgMaterial.flags */
#define DWG_MAT_DOUBLE_SIDED 1 /* draw both faces (all triangle materials) */
#define DWG_MAT_BLEND 2        /* alpha < 1: alpha blending */
#define DWG_MAT_UNLIT 4        /* lines: plain color, no lighting */

/* DwgSceneData.flags */
#define DWG_SCENE_Z_UP 1     /* the scene is Z-up (DWG_OPT_Z_UP) */
#define DWG_SCENE_HAS_BOUNDS 2 /* bounds_min/max are set (the scene is not empty) */

/* Conversion options. Set size = sizeof(DwgOptions) (dwg_options_init does
 * that); a zeroed struct with its size set is the default. */
typedef struct DwgOptions {
    uint32_t size;
    uint32_t flags;          /* DWG_OPT_* */
    int32_t units;           /* DWG_UNITS_DRAWING, DWG_UNITS_AUTO or an INSUNITS code */
    float crease_angle;      /* visible edges: degrees between two faces; 0 = 30 */
    uint32_t max_instances;  /* limits per drawing; 0 = built-in default */
    uint32_t max_triangles;
    uint32_t max_line_points;
    uint32_t reserved;       /* 0 */
} DwgOptions;

/* A draw call: a range of the mesh's index list with one material. Indices
 * are absolute (into the whole mesh's vertices); the vertices they use lie
 * in first_vertex .. first_vertex + vertex_count - 1. */
typedef struct DwgSubmesh {
    uint32_t topology;    /* DWG_TRIANGLES or DWG_LINES */
    uint32_t material;    /* index into DwgSceneData.materials */
    uint32_t first_index;
    uint32_t index_count;
    uint32_t first_vertex;
    uint32_t vertex_count;
} DwgSubmesh;

/* One mesh: a block's geometry for one inherited style (layer, color,
 * material), stored once and drawn by every instance that uses it. */
typedef struct DwgMesh {
    const char *name;           /* the block's name */
    const float *positions;     /* vertex_count * 3, block coordinates */
    const float *normals;       /* vertex_count * 3, unit length (0 for line vertices) */
    const float *uvs;           /* vertex_count * 2, glTF convention (v down); NULL
                                   when no submesh is textured */
    const uint32_t *indices;    /* index_count */
    const DwgSubmesh *submeshes;/* submesh_count */
    const uint32_t *edges;      /* edge_count * 2: line list into positions */
    uint32_t vertex_count;
    uint32_t index_count;
    uint32_t submesh_count;
    uint32_t edge_count;
    uint32_t triangle_count;
    uint32_t line_count;        /* line segments */
    float bounds_min[3];        /* of the positions */
    float bounds_max[3];
} DwgMesh;

/* A material. Base color textures have their texture transform applied to
 * the mesh UVs already; normal maps can have their own transform (glTF
 * KHR_texture_transform: uv' = T(offset) * R(rotation) * S(scale) * uv,
 * R(r) = [cos r, sin r; -sin r, cos r]), identity when not set. */
typedef struct DwgMaterial {
    const char *name;
    float base_color[4];     /* linear RGBA; [3] is the opacity */
    float metallic;
    float roughness;
    int32_t texture;         /* base color texture, index into textures, or -1 */
    int32_t normal_texture;  /* tangent-space normal map, or -1 */
    uint32_t flags;          /* DWG_MAT_* */
    float normal_uv_rotation;/* radians */
    float normal_uv_offset[2];
    float normal_uv_scale[2];
} DwgMaterial;

/* A texture image file as stored in the drawing (JPEG, PNG, ...). The
 * sampler always repeats in both directions. */
typedef struct DwgTexture {
    const char *name;
    const char *mime_type;   /* "image/jpeg", "image/png", ... */
    const uint8_t *data;
    uint64_t size;
} DwgTexture;

/* A node of the scene tree: the root (model space), a block instance
 * (INSERT), or a helper node holding a mesh moved near the origin. Parents
 * come before their children (parent < own index). */
typedef struct DwgNode {
    const char *name;        /* the block's name for an INSERT, "ModelSpace" for the root */
    const char *block_name;  /* the inserted block, "" for the root and helper nodes */
    const char *handle;      /* the INSERT's DWG handle (hex), or "" */
    int32_t parent;          /* node index, -1 for the root */
    int32_t mesh;            /* mesh index, or -1 */
    double local[16];        /* relative to the parent, column-major */
    double world[16];        /* local * parent's world, column-major */
} DwgNode;

/* A mesh to draw: every node with a mesh, its world matrix as float. */
typedef struct DwgInstance {
    uint32_t node;
    uint32_t mesh;
    float world[16];         /* column-major */
} DwgInstance;

/* The whole scene. */
typedef struct DwgSceneData {
    uint32_t size;           /* sizeof(DwgSceneData) of the library */
    uint32_t flags;          /* DWG_SCENE_* */
    uint32_t mesh_count;
    uint32_t material_count;
    uint32_t texture_count;
    uint32_t node_count;
    uint32_t instance_count;
    uint32_t warning_count;
    const DwgMesh *meshes;
    const DwgMaterial *materials;
    const DwgTexture *textures;
    const DwgNode *nodes;
    const DwgInstance *instances;
    const char *const *warnings;
    const char *summary;     /* what was converted, repaired, skipped (lines of text) */
    double bounds_min[3];    /* world, meters */
    double bounds_max[3];
    double unit_scale;       /* meters per drawing unit, as converted */
    int32_t insunits;        /* the drawing's INSUNITS code */
    uint32_t reserved;
    uint64_t triangles_stored; /* in the meshes */
    uint64_t triangles_drawn;  /* over all instances */
    uint64_t line_segments;    /* stored */
    uint64_t block_instances;  /* INSERTs (and MINSERT cells) converted */
    uint64_t edge_count;       /* visible edges, all meshes */
} DwgSceneData;

typedef struct DwgScene DwgScene;

/* Library version ("0.1.0"), static. */
DWG_API const char *dwg_version(void);
/* DWG_ABI of the library. */
DWG_API uint32_t dwg_abi_version(void);
/* The message of the last failed call on this thread ("" after success),
 * valid until the next call on this thread. */
DWG_API const char *dwg_last_error(void);

/* Fill *options with the defaults. */
DWG_API void dwg_options_init(DwgOptions *options);

/* Convert a DWG file (path in UTF-8); material images are also looked up
 * next to it. options may be NULL (defaults). On success *out is the scene,
 * else NULL and the result is a DWG_ERR_* code. */
DWG_API int32_t dwg_scene_open_file(const char *utf8_path, const DwgOptions *options,
                                    DwgScene **out);
/* The same for a file image in memory (copied); material images in separate
 * files are left out. */
DWG_API int32_t dwg_scene_open_memory(const uint8_t *data, size_t len,
                                      const DwgOptions *options, DwgScene **out);
/* Release a scene; NULL is ignored. */
DWG_API void dwg_scene_free(DwgScene *scene);

/* The scene's data (owned by the scene); NULL for NULL. */
DWG_API const DwgSceneData *dwg_scene_data(const DwgScene *scene);

/* The scene as a GLB file, byte for byte what the dwg2gltf command writes
 * with the same options (owned by the scene, made on the first call). */
DWG_API int32_t dwg_scene_to_glb(const DwgScene *scene, const uint8_t **data, size_t *len);

#ifdef __cplusplus
}
#endif

#endif /* DWG2GLTF_H */
