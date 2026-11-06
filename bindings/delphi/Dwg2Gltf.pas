// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf - Delphi (Win64) binding of dwg2gltf.dll, see include/dwg2gltf.h
  for the full description of every struct and function.

  Usage:
    var
      Options: TDwgOptions;
      Scene: PDwgScene;
      Data: PDwgSceneData;
    begin
      dwg_options_init(@Options);
      if dwg_scene_open_file(PAnsiChar(UTF8Encode(FileName)), @Options, Scene) <> DWG_OK then
        raise Exception.Create(DwgLastError);
      try
        Data := dwg_scene_data(Scene);
        for I := 0 to Integer(Data.instance_count) - 1 do
          DrawMesh(Data.meshes[Data.instances[I].mesh], Data.instances[I].world);
      finally
        dwg_scene_free(Scene);
      end;
    end;

  The scene is Y-up in meters (glTF's frame); matrices are column-major,
  which is the memory order of a Direct3D row-vector matrix (translation in
  elements 12..14). Base colors are linear RGBA. Strings are UTF-8 (use
  DwgStr). Pointers into a scene are valid until dwg_scene_free.

  The records use natural alignment, the same as the C compiler's on Win64:
  every field already sits at a multiple of its size, so no packing
  directive is involved ($ALIGN 8 is set for clarity). Typed pointers can
  be indexed (POINTERMATH), e.g. Mesh.positions[3 * I + 1]. }
unit Dwg2Gltf;

{$IFDEF FPC}
  {$MODE DELPHIUNICODE}
{$ENDIF}
{$ALIGN 8}
{$MINENUMSIZE 4}
{$POINTERMATH ON}

interface

const
{$IFDEF MSWINDOWS}
  DWG2GLTF_DLL = 'dwg2gltf.dll';
{$ELSE}
  DWG2GLTF_DLL = 'dwg2gltf'; // libdwg2gltf.so (Free Pascal on Linux)
{$ENDIF}

  DWG_ABI = 1;

  DWG_OK = 0;
  DWG_ERR_ARGUMENT = 1;
  DWG_ERR_IO = 2;
  DWG_ERR_READ = 3;
  DWG_ERR_NO_MODEL_SPACE = 4;
  DWG_ERR_EMPTY = 5;
  DWG_ERR_NON_FINITE = 6;
  DWG_ERR_INTERNAL = 7;

  DWG_OPT_NO_LINES = 1;
  DWG_OPT_PLAN_SYMBOLS = 2;
  DWG_OPT_NO_EDGES = 4;
  DWG_OPT_Z_UP = 8;
  DWG_OPT_EMPTY_IS_ERROR = 16;

  DWG_UNITS_DRAWING = 0;
  DWG_UNITS_AUTO = -1;

  DWG_TRIANGLES = 0;
  DWG_LINES = 1;

  DWG_MAT_DOUBLE_SIDED = 1;
  DWG_MAT_BLEND = 2;
  DWG_MAT_UNLIT = 4;

  DWG_SCENE_Z_UP = 1;
  DWG_SCENE_HAS_BOUNDS = 2;

type
  PDwgScene = Pointer;
  PPAnsiCharArray = ^PAnsiChar;
  PDwgIndex = ^UInt32;

  TDwgMatrixF = array[0..15] of Single;
  TDwgMatrixD = array[0..15] of Double;

  TDwgOptions = record
    size: UInt32;
    flags: UInt32;
    units: Int32;
    crease_angle: Single;
    max_instances: UInt32;
    max_triangles: UInt32;
    max_line_points: UInt32;
    reserved: UInt32;
  end;
  PDwgOptions = ^TDwgOptions;

  TDwgSubmesh = record
    topology: UInt32;
    material: UInt32;
    first_index: UInt32;
    index_count: UInt32;
    first_vertex: UInt32;
    vertex_count: UInt32;
  end;
  PDwgSubmesh = ^TDwgSubmesh;

  TDwgMesh = record
    name: PAnsiChar;
    positions: PSingle;
    normals: PSingle;
    uvs: PSingle;
    indices: PDwgIndex;
    submeshes: PDwgSubmesh;
    edges: PDwgIndex;
    vertex_count: UInt32;
    index_count: UInt32;
    submesh_count: UInt32;
    edge_count: UInt32;
    triangle_count: UInt32;
    line_count: UInt32;
    bounds_min: array[0..2] of Single;
    bounds_max: array[0..2] of Single;
  end;
  PDwgMesh = ^TDwgMesh;

  TDwgMaterial = record
    name: PAnsiChar;
    base_color: array[0..3] of Single;
    metallic: Single;
    roughness: Single;
    texture: Int32;
    normal_texture: Int32;
    flags: UInt32;
    normal_uv_rotation: Single;
    normal_uv_offset: array[0..1] of Single;
    normal_uv_scale: array[0..1] of Single;
  end;
  PDwgMaterial = ^TDwgMaterial;

  TDwgTexture = record
    name: PAnsiChar;
    mime_type: PAnsiChar;
    data: PByte;
    size: UInt64;
  end;
  PDwgTexture = ^TDwgTexture;

  TDwgNode = record
    name: PAnsiChar;
    block_name: PAnsiChar;
    handle: PAnsiChar;
    parent: Int32;
    mesh: Int32;
    local: TDwgMatrixD;
    world: TDwgMatrixD;
  end;
  PDwgNode = ^TDwgNode;

  TDwgInstance = record
    node: UInt32;
    mesh: UInt32;
    world: TDwgMatrixF;
  end;
  PDwgInstance = ^TDwgInstance;

  TDwgSceneData = record
    size: UInt32;
    flags: UInt32;
    mesh_count: UInt32;
    material_count: UInt32;
    texture_count: UInt32;
    node_count: UInt32;
    instance_count: UInt32;
    warning_count: UInt32;
    meshes: PDwgMesh;
    materials: PDwgMaterial;
    textures: PDwgTexture;
    nodes: PDwgNode;
    instances: PDwgInstance;
    warnings: PPAnsiCharArray;
    summary: PAnsiChar;
    bounds_min: array[0..2] of Double;
    bounds_max: array[0..2] of Double;
    unit_scale: Double;
    insunits: Int32;
    reserved: UInt32;
    triangles_stored: UInt64;
    triangles_drawn: UInt64;
    line_segments: UInt64;
    block_instances: UInt64;
    edge_count: UInt64;
  end;
  PDwgSceneData = ^TDwgSceneData;

function dwg_version: PAnsiChar; cdecl; external DWG2GLTF_DLL;
function dwg_abi_version: UInt32; cdecl; external DWG2GLTF_DLL;
function dwg_last_error: PAnsiChar; cdecl; external DWG2GLTF_DLL;
procedure dwg_options_init(options: PDwgOptions); cdecl; external DWG2GLTF_DLL;
function dwg_scene_open_file(utf8_path: PAnsiChar; options: PDwgOptions;
  out scene: PDwgScene): Int32; cdecl; external DWG2GLTF_DLL;
function dwg_scene_open_memory(data: Pointer; len: NativeUInt; options: PDwgOptions;
  out scene: PDwgScene): Int32; cdecl; external DWG2GLTF_DLL;
procedure dwg_scene_free(scene: PDwgScene); cdecl; external DWG2GLTF_DLL;
function dwg_scene_data(scene: PDwgScene): PDwgSceneData; cdecl; external DWG2GLTF_DLL;
function dwg_scene_to_glb(scene: PDwgScene; out data: PByte;
  out len: NativeUInt): Int32; cdecl; external DWG2GLTF_DLL;

{ A UTF-8 string from the library as a Delphi string. }
function DwgStr(P: PAnsiChar): string;
{ dwg_last_error as a Delphi string. }
function DwgLastError: string;
{ dwg_scene_open_file with a Delphi file name. }
function DwgOpenFile(const FileName: string; Options: PDwgOptions;
  out Scene: PDwgScene): Int32;
{ The I-th warning of a scene. }
function DwgWarning(const Data: TDwgSceneData; I: Integer): string;

implementation

function DwgStr(P: PAnsiChar): string;
begin
  if P = nil then
    Result := ''
  else
    Result := UTF8ToString(P);
end;

function DwgLastError: string;
begin
  Result := DwgStr(dwg_last_error);
end;

function DwgOpenFile(const FileName: string; Options: PDwgOptions;
  out Scene: PDwgScene): Int32;
var
  Path: UTF8String;
begin
  Path := UTF8String(FileName);
  Result := dwg_scene_open_file(PAnsiChar(Path), Options, Scene);
end;

function DwgWarning(const Data: TDwgSceneData; I: Integer): string;
begin
  Result := DwgStr(Data.warnings[I]);
end;

end.
