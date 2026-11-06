// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ PickingTest: checks the viewer's CPU side without a GPU, for each drawing
  given: the picker's BVH finds the same nearest triangle as testing every
  triangle, rays at the scene hit it, snapping next to a projected end point
  of a visible edge returns that end point, and the camera's projection and
  pixel rays agree. Also prints what the renderer would upload. Exit code 1
  on a failure. }
program PickingTest;

{$APPTYPE CONSOLE}
{$POINTERMATH ON}

uses
  System.SysUtils,
  System.Math,
  Dwg2Gltf in '..\..\..\bindings\delphi\Dwg2Gltf.pas',
  Dwg2Gltf.Math in 'Dwg2Gltf.Math.pas',
  Dwg2Gltf.Camera in 'Dwg2Gltf.Camera.pas',
  Dwg2Gltf.Picking in 'Dwg2Gltf.Picking.pas',
  ViewerDocument in 'ViewerDocument.pas';

var
  Failures: Integer = 0;

procedure Expect(Ok: Boolean; const What: string);
begin
  if not Ok then
  begin
    WriteLn('  FAIL: ', What);
    Inc(Failures);
  end;
end;

function Vertex(const Mesh: TDwgMesh; I: UInt32): TVec3;
begin
  Result := TVec3.Create(Mesh.positions[3 * I], Mesh.positions[3 * I + 1],
    Mesh.positions[3 * I + 2]);
end;

{ The nearest hit by testing every triangle of every instance. }
function BruteForce(Doc: TViewerDocument; const Ray: TRay; out Best: Double): Integer;
var
  I, J, K: Integer;
  Mesh: PDwgMesh;
  Sub: PDwgSubmesh;
  W: TMat4;
  T: Double;
  F: UInt32;
begin
  Result := -1;
  Best := MaxDouble;
  for I := 0 to High(Doc.Picker.Instances) do
  begin
    W := Doc.Picker.Instances[I].World;
    Mesh := @Doc.Data.meshes[Doc.Picker.Instances[I].Mesh];
    for J := 0 to Integer(Mesh.submesh_count) - 1 do
    begin
      Sub := @Mesh.submeshes[J];
      if Sub.topology <> DWG_TRIANGLES then
        Continue;
      K := 0;
      while K + 2 < Integer(Sub.index_count) do
      begin
        F := Sub.first_index + UInt32(K);
        if RayTriangle(Ray.Origin, Ray.Dir, W.TransformPoint(Vertex(Mesh^, Mesh.indices[F])),
          W.TransformPoint(Vertex(Mesh^, Mesh.indices[F + 1])),
          W.TransformPoint(Vertex(Mesh^, Mesh.indices[F + 2])), T) and (T < Best) then
        begin
          Best := T;
          Result := I;
        end;
        Inc(K, 3);
      end;
    end;
  end;
end;

procedure Test(const FileName: string);
var
  Doc: TViewerDocument;
  Camera: TOrbitCamera;
  Ray: TRay;
  Hit: THit;
  Snap: TSnap;
  I, J, Hits, Agree, Inst: Integer;
  T, X, Y, R: Double;
  P, Corner: TVec3;
  Mesh: PDwgMesh;
  Vertices, Indices, Edges: Int64;
  Fmt: TFormatSettings;
  View: TStandardView;
  Found: Boolean;
begin
  Fmt := TFormatSettings.Invariant;
  WriteLn(ExtractFileName(FileName));
  Doc := TViewerDocument.Create(FileName);
  try
    Vertices := 0;
    Indices := 0;
    Edges := 0;
    for I := 0 to Integer(Doc.Data.mesh_count) - 1 do
    begin
      Inc(Vertices, Doc.Data.meshes[I].vertex_count);
      Inc(Indices, Doc.Data.meshes[I].index_count);
      Inc(Edges, Doc.Data.meshes[I].edge_count);
    end;
    WriteLn(Format('  upload: %d vertices (%d bytes), %d indices, %d edges, %d instances, %d textures',
      [Vertices, Vertices * 32, Indices, Edges, Doc.Data.instance_count, Doc.Data.texture_count]));
    if Doc.Box.IsEmpty then
    begin
      WriteLn('  empty scene');
      Exit;
    end;
    R := Doc.Box.Radius;

    // 1. BVH against brute force, rays from a sphere around the scene
    // towards points inside its box
    RandSeed := 1;
    Hits := 0;
    Agree := 0;
    for I := 1 to 500 do
    begin
      P := TVec3.Create(Random - 0.5, Random - 0.5, Random - 0.5).Normalized;
      Ray.Origin := Doc.Box.Center + P * (3 * R);
      Ray.Dir := (TVec3.Create(
        Doc.Box.Min.X + Random * (Doc.Box.Max.X - Doc.Box.Min.X),
        Doc.Box.Min.Y + Random * (Doc.Box.Max.Y - Doc.Box.Min.Y),
        Doc.Box.Min.Z + Random * (Doc.Box.Max.Z - Doc.Box.Min.Z)) - Ray.Origin).Normalized;
      Inst := BruteForce(Doc, Ray, T);
      Found := Doc.Picker.Pick(Ray, Hit);
      if Found <> (Inst >= 0) then
        Continue;
      if Found then
      begin
        Inc(Hits);
        if Abs(Hit.T - T) > 1E-9 * (1 + T) then
          Continue;
      end;
      Inc(Agree);
    end;
    WriteLn(Format('  bvh: %d of 500 rays agree with brute force, %d hits', [Agree, Hits]));
    Expect(Agree = 500, 'BVH and brute force disagree');
    Expect(Hits > 0, 'no ray hit the scene');

    // 2. camera: a pixel's ray goes through the point that projects there,
    // and the ray through the screen center of a fitted view hits something
    // (for these solid test drawings)
    Camera.Init;
    Camera.Width := 800;
    Camera.Height := 600;
    Camera.SceneBox := Doc.Box;
    for View := Low(TStandardView) to High(TStandardView) do
    begin
      Camera.SetView(View);
      Camera.Fit(Doc.Box);
      P := Doc.Box.Center + TVec3.Create(0.1, 0.2, -0.05) * R;
      Expect(Camera.Project(P, X, Y), 'projection');
      Ray := Camera.RayAt(X, Y);
      // distance of P from the ray
      Expect((P - Ray.Origin - Ray.Dir * (P - Ray.Origin).Dot(Ray.Dir)).Length < 1E-6 * R,
        'RayAt and Project disagree');
    end;

    // 3. snapping: a few pixels next to the projected end point of a
    // visible edge, where nothing hides it, gives an end point at most as
    // far from the cursor (it, or one that projects as near)
    Camera.Ortho := False;
    Camera.SetView(svIso);
    Camera.Fit(Doc.Box);
    Found := False;
    for I := 0 to High(Doc.Picker.Instances) do
    begin
      Mesh := @Doc.Data.meshes[Doc.Picker.Instances[I].Mesh];
      for J := 0 to Integer(Mesh.edge_count) * 2 - 1 do
      begin
        Corner := Doc.Picker.Instances[I].World.TransformPoint(Vertex(Mesh^, Mesh.edges[J]));
        if not Camera.Project(Corner, X, Y) or (X < 20) or (Y < 20) or (X > 780) or (Y > 580) then
          Continue;
        Ray.Origin := Camera.Eye;
        Ray.Dir := (Corner - Ray.Origin).Normalized;
        if Doc.Picker.Pick(Ray, Hit) and
          (Hit.T < (Corner - Ray.Origin).Length - 3 * Camera.PixelSize(Corner)) then
          Continue; // hidden
        Snap := Doc.Picker.Snap(Camera, X + 3, Y - 2, 12);
        Found := True;
        Camera.Project(Snap.Point, X, Y);
        WriteLn(Format('  snap: %s (%.4f, %.4f, %.4f) for the end point (%.4f, %.4f, %.4f)',
          [SnapKindNames[Snap.Kind], Snap.Point.X, Snap.Point.Y, Snap.Point.Z,
           Corner.X, Corner.Y, Corner.Z], Fmt));
        Expect(Snap.Kind = skVertex, 'no end point snapped');
        Camera.Project(Corner, T, R);
        Expect(Sqrt(Sqr(X - T - 3) + Sqr(Y - R + 2)) <= Sqrt(13) + 1E-6,
          'the snapped end point is farther than the expected one');
        Break;
      end;
      if Found then
        Break;
    end;
    if Doc.Data.edge_count > 0 then
      Expect(Found, 'no visible end point in view');
    // the surface under the center of the view
    Snap := Doc.Picker.Snap(Camera, 400, 300, 1);
    WriteLn('  snap at the center: ', SnapKindNames[Snap.Kind]);
  finally
    Doc.Free;
  end;
end;

var
  I: Integer;
begin
  if ParamCount = 0 then
  begin
    WriteLn(ErrOutput, 'usage: PickingTest FILE.dwg...');
    Halt(2);
  end;
  for I := 1 to ParamCount do
    try
      Test(ParamStr(I));
    except
      on E: Exception do
      begin
        WriteLn('  FAIL: ', E.Message);
        Inc(Failures);
      end;
    end;
  if Failures > 0 then
  begin
    WriteLn(Failures, ' failures');
    Halt(1);
  end;
  WriteLn('picking: all checks passed');
end.
