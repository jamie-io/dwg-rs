// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ ViewerDocument - an open drawing: the scene from dwg2gltf.dll, the origin
  the viewer works around, the picker and the node tree, independent of the
  window and of the GPU (the renderer uploads from it). }
unit ViewerDocument;

interface

uses
  System.SysUtils, Dwg2Gltf, Dwg2Gltf.Math, Dwg2Gltf.Picking;

type
  TViewerDocument = class
  private
    FScene: PDwgScene;
    FData: PDwgSceneData;
    FFileName: string;
    FOrigin: TVec3;
    FBox: TBox;
    FPicker: TDwgPicker;
    FFirstChild, FNextSibling: TArray<Integer>;
    FLoadSeconds: Double;
  public
    { Convert the drawing; raises an exception with the library's message. }
    constructor Create(const FileName: string);
    destructor Destroy; override;
    { Per instance: does it belong to node Node's subtree? Node -1: none. }
    function SubtreeInstances(Node: Integer): TArray<Boolean>;
    { The node label for the block tree. }
    function NodeLabel(Node: Integer): string;
    property Data: PDwgSceneData read FData;
    property FileName: string read FFileName;
    { Center of the scene's bounds: everything the viewer computes is
      relative to it (see Dwg2Gltf.Renderer). }
    property Origin: TVec3 read FOrigin;
    { Bounds relative to Origin. }
    property Box: TBox read FBox;
    property Picker: TDwgPicker read FPicker;
    { Children of a node: FirstChild[N], then NextSibling until -1. }
    property FirstChild: TArray<Integer> read FFirstChild;
    property NextSibling: TArray<Integer> read FNextSibling;
    property LoadSeconds: Double read FLoadSeconds;
  end;

implementation

uses
  System.Diagnostics;

constructor TViewerDocument.Create(const FileName: string);
var
  Options: TDwgOptions;
  Watch: TStopwatch;
  I, Parent: Integer;
  LastChild: TArray<Integer>;
begin
  inherited Create;
  FFileName := FileName;
  dwg_options_init(@Options);
  Watch := TStopwatch.StartNew;
  if DwgOpenFile(FileName, @Options, FScene) <> DWG_OK then
    raise Exception.CreateFmt('%s: %s', [ExtractFileName(FileName), DwgLastError]);
  FLoadSeconds := Watch.Elapsed.TotalSeconds;
  FData := dwg_scene_data(FScene);

  if FData.flags and DWG_SCENE_HAS_BOUNDS <> 0 then
    FOrigin := TVec3.Create(
      (FData.bounds_min[0] + FData.bounds_max[0]) / 2,
      (FData.bounds_min[1] + FData.bounds_max[1]) / 2,
      (FData.bounds_min[2] + FData.bounds_max[2]) / 2)
  else
    FOrigin := TVec3.Create(0, 0, 0);
  FPicker := TDwgPicker.Create(FData^, FOrigin);
  FBox := FPicker.SceneBox;

  // children lists, in the scene's order (parents come before children)
  SetLength(FFirstChild, FData.node_count);
  SetLength(FNextSibling, FData.node_count);
  SetLength(LastChild, FData.node_count);
  for I := 0 to High(FFirstChild) do
  begin
    FFirstChild[I] := -1;
    FNextSibling[I] := -1;
    LastChild[I] := -1;
  end;
  for I := 0 to High(FFirstChild) do
  begin
    Parent := FData.nodes[I].parent;
    if (Parent < 0) or (Parent >= I) then
      Continue;
    if LastChild[Parent] < 0 then
      FFirstChild[Parent] := I
    else
      FNextSibling[LastChild[Parent]] := I;
    LastChild[Parent] := I;
  end;
end;

destructor TViewerDocument.Destroy;
begin
  FPicker.Free;
  dwg_scene_free(FScene);
  inherited;
end;

function TViewerDocument.SubtreeInstances(Node: Integer): TArray<Boolean>;
var
  InTree: TArray<Boolean>;
  I, Parent: Integer;
begin
  SetLength(Result, FData.instance_count);
  if Node < 0 then
    Exit;
  // one pass: parents come before their children
  SetLength(InTree, FData.node_count);
  for I := 0 to High(InTree) do
  begin
    Parent := FData.nodes[I].parent;
    InTree[I] := (I = Node) or ((Parent >= 0) and (Parent < I) and InTree[Parent]);
  end;
  for I := 0 to High(Result) do
    Result[I] := InTree[FData.instances[I].node];
end;

function TViewerDocument.NodeLabel(Node: Integer): string;
var
  N: PDwgNode;
begin
  N := @FData.nodes[Node];
  Result := DwgStr(N.name);
  if Result = '' then
    Result := DwgStr(N.block_name);
  if Result = '' then
    Result := '(unnamed)';
  if (N.handle <> nil) and (N.handle^ <> #0) then
    Result := Result + '  #' + DwgStr(N.handle);
  if N.mesh >= 0 then
    Result := Result + Format('  [%d triangles]', [FData.meshes[N.mesh].triangle_count]);
end;

end.
