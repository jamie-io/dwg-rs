// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ SceneInfo: open DWG files through dwg2gltf.dll and print what a viewer
  gets (the Delphi counterpart of examples/c/scene_info.c).
  Build: dcc64 SceneInfo.dpr, and put dwg2gltf.dll next to SceneInfo.exe
  (Free Pascal on Linux: fpc SceneInfo.dpr, against libdwg2gltf.so). }
program SceneInfo;

{$IFDEF MSWINDOWS}
  {$APPTYPE CONSOLE}
{$ENDIF}
{$IFDEF FPC}
  {$MODE DELPHIUNICODE}
{$ENDIF}

uses
  SysUtils,
  Dwg2Gltf in '..\..\bindings\delphi\Dwg2Gltf.pas';

function Show(const FileName: string): Boolean;
var
  Options: TDwgOptions;
  Scene: PDwgScene;
  Data: PDwgSceneData;
  Mesh: PDwgMesh;
  Node: PDwgNode;
  Glb: PByte;
  GlbLen: NativeUInt;
  I, Status: Integer;
  Vertices, Submeshes: Cardinal;
  Fmt: TFormatSettings;
begin
  Fmt := FormatSettings;
  Fmt.DecimalSeparator := '.';
  dwg_options_init(@Options);
  Status := DwgOpenFile(FileName, @Options, Scene);
  if Status <> DWG_OK then
  begin
    WriteLn(ErrOutput, FileName, ': error ', Status, ': ', DwgLastError);
    Exit(False);
  end;
  try
    Data := dwg_scene_data(Scene);
    Vertices := 0;
    Submeshes := 0;
    for I := 0 to Integer(Data.mesh_count) - 1 do
    begin
      Inc(Vertices, Data.meshes[I].vertex_count);
      Inc(Submeshes, Data.meshes[I].submesh_count);
    end;
    WriteLn(FileName);
    WriteLn(Format('  meshes: %d (%d submeshes, %d vertices)',
      [Data.mesh_count, Submeshes, Vertices]));
    WriteLn(Format('  nodes: %d, instances: %d, materials: %d, textures: %d',
      [Data.node_count, Data.instance_count, Data.material_count, Data.texture_count]));
    WriteLn(Format('  triangles: %d stored, %d drawn; line segments: %d; visible edges: %d',
      [Data.triangles_stored, Data.triangles_drawn, Data.line_segments, Data.edge_count]));
    WriteLn(Format('  units: INSUNITS %d, %g m per unit',
      [Data.insunits, Data.unit_scale], Fmt));
    if Data.flags and DWG_SCENE_HAS_BOUNDS <> 0 then
      WriteLn(Format('  bounds: (%.4f %.4f %.4f) .. (%.4f %.4f %.4f) m',
        [Data.bounds_min[0], Data.bounds_min[1], Data.bounds_min[2],
         Data.bounds_max[0], Data.bounds_max[1], Data.bounds_max[2]], Fmt));
    for I := 0 to Integer(Data.mesh_count) - 1 do
    begin
      if I = 5 then Break;
      Mesh := @Data.meshes[I];
      WriteLn(Format('  mesh %d "%s": %d triangles, %d lines, %d edges',
        [I, DwgStr(Mesh.name), Mesh.triangle_count, Mesh.line_count, Mesh.edge_count]));
    end;
    for I := 0 to Integer(Data.node_count) - 1 do
    begin
      if I = 5 then Break;
      Node := @Data.nodes[I];
      WriteLn(Format('  node %d "%s" parent %d mesh %d block "%s"',
        [I, DwgStr(Node.name), Node.parent, Node.mesh, DwgStr(Node.block_name)]));
    end;
    for I := 0 to Integer(Data.warning_count) - 1 do
      WriteLn('  warning: ', DwgWarning(Data^, I));
    if dwg_scene_to_glb(Scene, Glb, GlbLen) = DWG_OK then
      WriteLn(Format('  glb: %d bytes', [GlbLen]))
    else
      WriteLn(ErrOutput, FileName, ': glb: ', DwgLastError);
    Result := True;
  finally
    dwg_scene_free(Scene);
  end;
end;

var
  I: Integer;
  Failed: Boolean;
begin
  if ParamCount = 0 then
  begin
    WriteLn(ErrOutput, 'usage: SceneInfo FILE.dwg...');
    Halt(2);
  end;
  WriteLn('dwg2gltf ', DwgStr(dwg_version), ' (ABI ', dwg_abi_version, ')');
  if dwg_abi_version <> DWG_ABI then
  begin
    WriteLn(ErrOutput, 'dwg2gltf.dll has another ABI version than Dwg2Gltf.pas');
    Halt(1);
  end;
  Failed := False;
  for I := 1 to ParamCount do
    if not Show(ParamStr(I)) then
      Failed := True;
  if Failed then
    Halt(1);
end.
