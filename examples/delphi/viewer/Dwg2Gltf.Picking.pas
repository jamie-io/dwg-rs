// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf.Picking - picking and snapping on the CPU, without the GPU.

  Pick: the ray is tested against each instance's box in the scene, then,
  transformed into the mesh's own coordinates, against a bounding volume
  hierarchy of the mesh's triangles (built on first use, shared by every
  instance of the mesh). Distances along the ray stay in scene units: an
  affine transform keeps the ray parameter.

  Snap: around the cursor (a radius in pixels) the visible edges of
  dwg2gltf.dll and the drawing's line geometry are searched for an end
  point, then a midpoint, then the nearest point on an edge; a candidate
  hidden behind other geometry is skipped. Without one, the surface point
  under the cursor is taken.

  The picker reads the scene's arrays directly: keep the DwgScene open
  while the picker lives. Coordinates are relative to Origin, as in
  Dwg2Gltf.Renderer. }
unit Dwg2Gltf.Picking;

{$POINTERMATH ON}

interface

uses
  System.SysUtils, Dwg2Gltf, Dwg2Gltf.Math, Dwg2Gltf.Camera;

type
  TSnapKind = (skNone, skSurface, skVertex, skMidpoint, skEdge);

  THit = record
    Instance: Integer;
    Triangle: Integer; // first index of the triangle in the mesh's index list
    T: Double;         // distance along the ray
    Point: TVec3;
  end;

  TSnap = record
    Kind: TSnapKind;
    Point: TVec3;
    Instance: Integer;
  end;

  TBvhNode = record
    BoxMin, BoxMax: TVec3;
    First: Integer; // leaf: first item in Tris; interior: index of the left child
    Count: Integer; // leaf: triangle count; interior: 0
  end;

  TMeshBvh = record
    Built: Boolean;
    Nodes: TArray<TBvhNode>;
    Tris: TArray<UInt32>; // first index of each triangle, in leaf order
  end;

  TPickInstance = record
    Mesh: Integer;
    World, Inverse: TMat4;
    Box: TBox; // in the scene
  end;

  TDwgPicker = class
  private
    FData: PDwgSceneData;
    FInstances: TArray<TPickInstance>;
    FBvh: TArray<TMeshBvh>;
    FSceneBox: TBox;
    procedure BuildBvh(MeshIndex: Integer);
    function Vertex(const Mesh: TDwgMesh; Index: UInt32): TVec3; inline;
    function PickMesh(MeshIndex: Integer; const Local: TRay; var Best: Double;
      out Tri: Integer): Boolean;
    function Visible(const Camera: TOrbitCamera; const P: TVec3): Boolean;
  public
    constructor Create(const Data: TDwgSceneData; const Origin: TVec3);
    { The nearest triangle along the ray. }
    function Pick(const Ray: TRay; out Hit: THit): Boolean;
    { The snap point for the cursor at (X, Y) within Radius pixels. }
    function Snap(const Camera: TOrbitCamera; X, Y, Radius: Double): TSnap;
    property SceneBox: TBox read FSceneBox;
    property Instances: TArray<TPickInstance> read FInstances;
  end;

const
  SnapKindNames: array[TSnapKind] of string =
    ('none', 'surface', 'end point', 'midpoint', 'edge');

implementation

uses
  System.Math, System.Generics.Collections, System.Generics.Defaults;

const
  LeafSize = 4;

constructor TDwgPicker.Create(const Data: TDwgSceneData; const Origin: TVec3);
var
  I: Integer;
  Mesh: PDwgMesh;
  Node: PDwgNode;
  MeshBox: TBox;
begin
  inherited Create;
  FData := @Data;
  SetLength(FBvh, Data.mesh_count);
  SetLength(FInstances, Data.instance_count);
  FSceneBox.Clear;
  for I := 0 to High(FInstances) do
  begin
    FInstances[I].Mesh := Data.instances[I].mesh;
    Node := @Data.nodes[Data.instances[I].node];
    FInstances[I].World := TMat4.FromDoubles(Node.world);
    FInstances[I].World.M[12] := FInstances[I].World.M[12] - Origin.X;
    FInstances[I].World.M[13] := FInstances[I].World.M[13] - Origin.Y;
    FInstances[I].World.M[14] := FInstances[I].World.M[14] - Origin.Z;
    FInstances[I].Inverse := FInstances[I].World.Inverse;
    Mesh := @Data.meshes[FInstances[I].Mesh];
    MeshBox.Min := TVec3.Create(Mesh.bounds_min[0], Mesh.bounds_min[1], Mesh.bounds_min[2]);
    MeshBox.Max := TVec3.Create(Mesh.bounds_max[0], Mesh.bounds_max[1], Mesh.bounds_max[2]);
    if Mesh.vertex_count = 0 then
      MeshBox.Clear;
    FInstances[I].Box := MeshBox.Transformed(FInstances[I].World);
    if not FInstances[I].Box.IsEmpty then
    begin
      FSceneBox.Include(FInstances[I].Box.Min);
      FSceneBox.Include(FInstances[I].Box.Max);
    end;
  end;
end;

function TDwgPicker.Vertex(const Mesh: TDwgMesh; Index: UInt32): TVec3;
begin
  Result.X := Mesh.positions[3 * Index];
  Result.Y := Mesh.positions[3 * Index + 1];
  Result.Z := Mesh.positions[3 * Index + 2];
end;

procedure TDwgPicker.BuildBvh(MeshIndex: Integer);
var
  Mesh: PDwgMesh;
  Bvh: ^TMeshBvh;
  Centroids: TArray<TVec3>;
  Order: TArray<Integer>;
  Firsts: TArray<UInt32>;
  I, J, Count, NodeCount: Integer;
  Sub: PDwgSubmesh;

  // node Index covers Order[First .. First + Count - 1]
  procedure Build(Index, First, Count: Integer);
  var
    I, K, Axis, Half, Left: Integer;
    CMin, CMax, P, Ext: TVec3;
  begin
    with Bvh.Nodes[Index] do
    begin
      BoxMin := TVec3.Create(MaxDouble, MaxDouble, MaxDouble);
      BoxMax := TVec3.Create(-MaxDouble, -MaxDouble, -MaxDouble);
    end;
    CMin := Bvh.Nodes[Index].BoxMin;
    CMax := Bvh.Nodes[Index].BoxMax;
    for I := First to First + Count - 1 do
    begin
      for K := 0 to 2 do
      begin
        P := Vertex(Mesh^, Mesh.indices[Firsts[Order[I]] + UInt32(K)]);
        Bvh.Nodes[Index].BoxMin := TVec3.Create(Min(Bvh.Nodes[Index].BoxMin.X, P.X),
          Min(Bvh.Nodes[Index].BoxMin.Y, P.Y), Min(Bvh.Nodes[Index].BoxMin.Z, P.Z));
        Bvh.Nodes[Index].BoxMax := TVec3.Create(Max(Bvh.Nodes[Index].BoxMax.X, P.X),
          Max(Bvh.Nodes[Index].BoxMax.Y, P.Y), Max(Bvh.Nodes[Index].BoxMax.Z, P.Z));
      end;
      P := Centroids[Order[I]];
      CMin := TVec3.Create(Min(CMin.X, P.X), Min(CMin.Y, P.Y), Min(CMin.Z, P.Z));
      CMax := TVec3.Create(Max(CMax.X, P.X), Max(CMax.Y, P.Y), Max(CMax.Z, P.Z));
    end;
    if Count <= LeafSize then
    begin
      Bvh.Nodes[Index].First := First;
      Bvh.Nodes[Index].Count := Count;
      Exit;
    end;
    // split at the median of the centroids along the longest axis
    Ext := CMax - CMin;
    if (Ext.X >= Ext.Y) and (Ext.X >= Ext.Z) then
      Axis := 0
    else if Ext.Y >= Ext.Z then
      Axis := 1
    else
      Axis := 2;
    TArray.Sort<Integer>(Order, TComparer<Integer>.Construct(
      function(const A, B: Integer): Integer
      begin
        case Axis of
          0: Result := CompareValue(Centroids[A].X, Centroids[B].X);
          1: Result := CompareValue(Centroids[A].Y, Centroids[B].Y);
        else
          Result := CompareValue(Centroids[A].Z, Centroids[B].Z);
        end;
      end), First, Count);
    Half := Count div 2;
    Left := NodeCount;
    Inc(NodeCount, 2);
    Bvh.Nodes[Index].First := Left;
    Bvh.Nodes[Index].Count := 0;
    Build(Left, First, Half);
    Build(Left + 1, First + Half, Count - Half);
  end;

begin
  Bvh := @FBvh[MeshIndex];
  Bvh.Built := True;
  Mesh := @FData.meshes[MeshIndex];
  Count := 0;
  for J := 0 to Integer(Mesh.submesh_count) - 1 do
    if Mesh.submeshes[J].topology = DWG_TRIANGLES then
      Inc(Count, Mesh.submeshes[J].index_count div 3);
  if Count = 0 then
    Exit;
  SetLength(Firsts, Count);
  Count := 0;
  for J := 0 to Integer(Mesh.submesh_count) - 1 do
  begin
    Sub := @Mesh.submeshes[J];
    if Sub.topology <> DWG_TRIANGLES then
      Continue;
    I := 0;
    while I + 2 < Integer(Sub.index_count) do
    begin
      Firsts[Count] := Sub.first_index + UInt32(I);
      Inc(Count);
      Inc(I, 3);
    end;
  end;
  SetLength(Centroids, Length(Firsts));
  SetLength(Order, Length(Firsts));
  for I := 0 to High(Firsts) do
  begin
    Centroids[I] := (Vertex(Mesh^, Mesh.indices[Firsts[I]]) +
      Vertex(Mesh^, Mesh.indices[Firsts[I] + 1]) +
      Vertex(Mesh^, Mesh.indices[Firsts[I] + 2])) * (1 / 3);
    Order[I] := I;
  end;
  SetLength(Bvh.Nodes, 2 * Length(Firsts));
  NodeCount := 1;
  Build(0, 0, Length(Firsts));
  SetLength(Bvh.Nodes, NodeCount);
  SetLength(Bvh.Tris, Length(Firsts));
  for I := 0 to High(Order) do
    Bvh.Tris[I] := Firsts[Order[I]];
end;

function TDwgPicker.PickMesh(MeshIndex: Integer; const Local: TRay; var Best: Double;
  out Tri: Integer): Boolean;
var
  Bvh: ^TMeshBvh;
  Mesh: PDwgMesh;
  Stack: array[0..63] of Integer;
  Top, N, I: Integer;
  T0, T1, T: Double;
  F: UInt32;
begin
  Result := False;
  Tri := -1;
  if not FBvh[MeshIndex].Built then
    BuildBvh(MeshIndex);
  Bvh := @FBvh[MeshIndex];
  if Bvh.Nodes = nil then
    Exit;
  Mesh := @FData.meshes[MeshIndex];
  Top := 0;
  Stack[0] := 0;
  while Top >= 0 do
  begin
    N := Stack[Top];
    Dec(Top);
    if not RayBox(Local, Bvh.Nodes[N].BoxMin, Bvh.Nodes[N].BoxMax, Best, T0, T1) then
      Continue;
    if Bvh.Nodes[N].Count > 0 then
    begin
      for I := Bvh.Nodes[N].First to Bvh.Nodes[N].First + Bvh.Nodes[N].Count - 1 do
      begin
        F := Bvh.Tris[I];
        if RayTriangle(Local.Origin, Local.Dir, Vertex(Mesh^, Mesh.indices[F]),
          Vertex(Mesh^, Mesh.indices[F + 1]), Vertex(Mesh^, Mesh.indices[F + 2]), T) and
          (T < Best) then
        begin
          Best := T;
          Tri := F;
          Result := True;
        end;
      end;
    end
    else if Top < High(Stack) - 1 then // depth is about log2(triangles / 4)
    begin
      Inc(Top);
      Stack[Top] := Bvh.Nodes[N].First;
      Inc(Top);
      Stack[Top] := Bvh.Nodes[N].First + 1;
    end;
  end;
end;

function TDwgPicker.Pick(const Ray: TRay; out Hit: THit): Boolean;
var
  I, Tri: Integer;
  Best, T0, T1: Double;
  Local: TRay;
begin
  Result := False;
  Best := MaxDouble;
  Hit.Instance := -1;
  for I := 0 to High(FInstances) do
  begin
    if FInstances[I].Box.IsEmpty or
      not RayBox(Ray, FInstances[I].Box.Min, FInstances[I].Box.Max, Best, T0, T1) then
      Continue;
    // the ray in mesh coordinates; Dir is not unit length there, which
    // keeps T the distance in the scene
    Local.Origin := FInstances[I].Inverse.TransformPoint(Ray.Origin);
    Local.Dir := FInstances[I].Inverse.TransformVector(Ray.Dir);
    if PickMesh(FInstances[I].Mesh, Local, Best, Tri) then
    begin
      Hit.Instance := I;
      Hit.Triangle := Tri;
      Hit.T := Best;
      Result := True;
    end;
  end;
  if Result then
    Hit.Point := Ray.Origin + Ray.Dir * Hit.T;
end;

function TDwgPicker.Visible(const Camera: TOrbitCamera; const P: TVec3): Boolean;
var
  F, R, U: TVec3;
  Ray: TRay;
  Hit: THit;
  Dist: Double;
begin
  Camera.Basis(F, R, U);
  if Camera.Ortho then
  begin
    Dist := Camera.Distance + 2 * FSceneBox.Radius;
    Ray.Dir := F;
    Ray.Origin := P - F * Dist;
  end
  else
  begin
    Ray.Origin := Camera.Eye;
    Dist := (P - Ray.Origin).Length;
    Ray.Dir := (P - Ray.Origin).Normalized;
  end;
  // something in front by more than a few pixels' worth hides P
  Result := not Pick(Ray, Hit) or (Hit.T >= Dist - 3 * Camera.PixelSize(P));
end;

type
  TCandidate = record
    Kind: TSnapKind;
    Pixels: Double;
    Point: TVec3;
    Instance: Integer;
  end;

function TDwgPicker.Snap(const Camera: TOrbitCamera; X, Y, Radius: Double): TSnap;
var
  Ray: TRay;
  Hit: THit;
  HasHit: Boolean;
  Candidates: TList<TCandidate>;
  I, J, K, Tested: Integer;
  Mesh: PDwgMesh;
  Sub: PDwgSubmesh;
  C: TCandidate;
  SX, SY, X0, Y0, X1, Y1: Double;
  Behind: Boolean;
  P: TVec3;
  ViewProj: TMat4;

  // Camera.Project, without recomputing the matrix for every point
  function Project(const Point: TVec3; out PX, PY: Double): Boolean;
  var
    CX, CY, CZ, CW: Double;
  begin
    ViewProj.TransformClip(Point, CX, CY, CZ, CW);
    Result := CW > 1E-12;
    if Result then
    begin
      PX := (CX / CW + 1) * 0.5 * Camera.Width - 0.5;
      PY := (1 - CY / CW) * 0.5 * Camera.Height - 0.5;
    end;
  end;

  procedure Consider(Kind: TSnapKind; const Point: TVec3; Inst: Integer);
  var
    PX, PY, D: Double;
  begin
    if not Project(Point, PX, PY) then
      Exit;
    D := Sqrt(Sqr(PX - X) + Sqr(PY - Y));
    if D > Radius then
      Exit;
    C.Kind := Kind;
    C.Pixels := D;
    C.Point := Point;
    C.Instance := Inst;
    Candidates.Add(C);
  end;

  procedure Segment(Inst: Integer; const A, B: TVec3);
  var
    T, S: Double;
  begin
    Consider(skVertex, A, Inst);
    Consider(skVertex, B, Inst);
    Consider(skMidpoint, (A + B) * 0.5, Inst);
    RaySegment(Ray, A, B, T, S);
    if T > 0 then
      Consider(skEdge, A + (B - A) * S, Inst);
  end;

begin
  Result.Kind := skNone;
  Result.Instance := -1;
  Ray := Camera.RayAt(X, Y);
  ViewProj := Camera.ViewProj;
  HasHit := Pick(Ray, Hit);
  Candidates := TList<TCandidate>.Create;
  try
    for I := 0 to High(FInstances) do
    begin
      if FInstances[I].Box.IsEmpty then
        Continue;
      // the instance's box on screen, grown by the radius, must hold the cursor
      X0 := MaxDouble; Y0 := MaxDouble; X1 := -MaxDouble; Y1 := -MaxDouble;
      Behind := False;
      for K := 0 to 7 do
      begin
        if K and 1 = 0 then P.X := FInstances[I].Box.Min.X else P.X := FInstances[I].Box.Max.X;
        if K and 2 = 0 then P.Y := FInstances[I].Box.Min.Y else P.Y := FInstances[I].Box.Max.Y;
        if K and 4 = 0 then P.Z := FInstances[I].Box.Min.Z else P.Z := FInstances[I].Box.Max.Z;
        if not Project(P, SX, SY) then
        begin
          Behind := True; // partly behind the camera: keep it
          Break;
        end;
        X0 := Min(X0, SX); Y0 := Min(Y0, SY); X1 := Max(X1, SX); Y1 := Max(Y1, SY);
      end;
      if not Behind and ((X < X0 - Radius) or (X > X1 + Radius) or (Y < Y0 - Radius) or
        (Y > Y1 + Radius)) then
        Continue;
      Mesh := @FData.meshes[FInstances[I].Mesh];
      // visible edges
      for J := 0 to Integer(Mesh.edge_count) - 1 do
        Segment(I, FInstances[I].World.TransformPoint(Vertex(Mesh^, Mesh.edges[2 * J])),
          FInstances[I].World.TransformPoint(Vertex(Mesh^, Mesh.edges[2 * J + 1])));
      // line geometry
      for J := 0 to Integer(Mesh.submesh_count) - 1 do
      begin
        Sub := @Mesh.submeshes[J];
        if Sub.topology <> DWG_LINES then
          Continue;
        K := 0;
        while K + 1 < Integer(Sub.index_count) do
        begin
          Segment(I,
            FInstances[I].World.TransformPoint(Vertex(Mesh^, Mesh.indices[Sub.first_index + UInt32(K)])),
            FInstances[I].World.TransformPoint(Vertex(Mesh^, Mesh.indices[Sub.first_index + UInt32(K) + 1])));
          Inc(K, 2);
        end;
      end;
    end;

    // end points before midpoints before edges, each the nearest first
    Candidates.Sort(TComparer<TCandidate>.Construct(
      function(const A, B: TCandidate): Integer
      begin
        Result := Ord(A.Kind) - Ord(B.Kind);
        if Result = 0 then
          Result := CompareValue(A.Pixels, B.Pixels);
      end));
    Tested := 0;
    for C in Candidates do
    begin
      if Tested = 24 then
        Break;
      Inc(Tested);
      if Visible(Camera, C.Point) then
      begin
        Result.Kind := C.Kind;
        Result.Point := C.Point;
        Result.Instance := C.Instance;
        Exit;
      end;
    end;
  finally
    Candidates.Free;
  end;
  if HasHit then
  begin
    Result.Kind := skSurface;
    Result.Point := Hit.Point;
    Result.Instance := Hit.Instance;
  end;
end;

end.
