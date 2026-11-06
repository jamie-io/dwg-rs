// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf.Camera - an orbit camera for a Y-up scene: orbit, pan, zoom to
  the cursor, fit to a box, standard views, perspective or orthographic.

  The camera circles Target at Distance; Yaw turns about the Y axis (0 looks
  along -Z, from the front), Pitch lifts the eye (90 degrees looks straight
  down). In the glTF frame of dwg2gltf.dll the drawing's front view (looking
  along the DWG +Y axis) is Yaw 0, its top view Pitch 90. }
unit Dwg2Gltf.Camera;

interface

uses
  Dwg2Gltf.Math;

type
  TStandardView = (svFront, svBack, svLeft, svRight, svTop, svBottom, svIso);

  TOrbitCamera = record
    Target: TVec3;
    Distance: Double;
    Yaw, Pitch: Double;   // radians
    FovY: Double;         // radians, also sets the orthographic scale
    Ortho: Boolean;
    Width, Height: Integer; // viewport in pixels
    SceneBox: TBox;       // for the near and far planes
    procedure Init;
    function Eye: TVec3;
    { Unit vectors of the view: Forward (into the screen), Right, Up. }
    procedure Basis(out Forward, Right, Up: TVec3);
    function View: TMat4;
    function Projection: TMat4;
    function ViewProj: TMat4;
    { The ray through pixel (X, Y), (0, 0) top left. }
    function RayAt(X, Y: Double): TRay;
    { Pixel position of a point; False when it is behind the camera. }
    function Project(const P: TVec3; out X, Y: Double): Boolean;
    { World size of one pixel at point P. }
    function PixelSize(const P: TVec3): Double;
    procedure Orbit(DX, DY: Double);
    procedure Pan(DX, DY: Double);
    { Zoom by Factor (< 1 closer), keeping the point under pixel (X, Y). }
    procedure Zoom(Factor, X, Y: Double);
    procedure Fit(const Box: TBox);
    procedure SetView(V: TStandardView);
  end;

implementation

uses
  System.Math;

procedure TOrbitCamera.Init;
begin
  Target := TVec3.Create(0, 0, 0);
  Distance := 10;
  FovY := DegToRad(40);
  Ortho := False;
  Width := 1;
  Height := 1;
  SceneBox.Clear;
  SetView(svIso);
end;

function TOrbitCamera.Eye: TVec3;
var
  F, R, U: TVec3;
begin
  Basis(F, R, U);
  Result := Target - F * Distance;
end;

procedure TOrbitCamera.Basis(out Forward, Right, Up: TVec3);
var
  CP, SP, CY, SY: Double;
begin
  SinCos(Pitch, SP, CP);
  SinCos(Yaw, SY, CY);
  // the eye sits at Target + Distance * (CP * SY, SP, CP * CY)
  Forward := TVec3.Create(-CP * SY, -SP, -CP * CY);
  Right := TVec3.Create(CY, 0, -SY);
  Up := Right.Cross(Forward); // stays valid at Pitch = +-90 degrees
end;

function TOrbitCamera.View: TMat4;
var
  F, R, U: TVec3;
begin
  Basis(F, R, U);
  Result := LookAtRH(Target - F * Distance, Target, U);
end;

function TOrbitCamera.Projection: TMat4;
var
  Aspect, ZNear, ZFar, D, Radius, H: Double;
  F, R, U: TVec3;
begin
  Aspect := Max(Width, 1) / Max(Height, 1);
  // near and far around the scene's bounding sphere: a tight range keeps
  // the depth buffer precise
  if SceneBox.IsEmpty then
  begin
    D := Distance;
    Radius := Distance;
  end
  else
  begin
    Basis(F, R, U);
    D := (SceneBox.Center - Eye).Dot(F);
    Radius := SceneBox.Radius * 1.05 + 1E-6;
  end;
  ZFar := Max(D + Radius, Distance * 1E-3);
  if Ortho then
  begin
    ZNear := D - Radius;
    H := 2 * Distance * Tan(FovY / 2);
    Result := OrthoRH(H * Aspect, H, ZNear, ZFar);
  end
  else
  begin
    ZNear := Max(D - Radius, ZFar * 1E-4);
    Result := PerspectiveRH(FovY, Aspect, ZNear, ZFar);
  end;
end;

function TOrbitCamera.ViewProj: TMat4;
begin
  Result := Projection * View;
end;

function TOrbitCamera.RayAt(X, Y: Double): TRay;
var
  F, R, U: TVec3;
  NX, NY, H: Double;
begin
  Basis(F, R, U);
  NX := (2 * (X + 0.5) / Max(Width, 1) - 1) * Max(Width, 1) / Max(Height, 1);
  NY := 1 - 2 * (Y + 0.5) / Max(Height, 1);
  H := Tan(FovY / 2);
  if Ortho then
  begin
    // parallel rays, starting well in front of the scene
    Result.Dir := F;
    Result.Origin := Target + R * (NX * H * Distance) + U * (NY * H * Distance) -
      F * (Distance + 2 * SceneBox.Radius);
  end
  else
  begin
    Result.Origin := Eye;
    Result.Dir := (F + R * (NX * H) + U * (NY * H)).Normalized;
  end;
end;

function TOrbitCamera.Project(const P: TVec3; out X, Y: Double): Boolean;
var
  CX, CY, CZ, CW: Double;
begin
  ViewProj.TransformClip(P, CX, CY, CZ, CW);
  Result := CW > 1E-12;
  if not Result then
    Exit;
  X := (CX / CW + 1) * 0.5 * Width - 0.5;
  Y := (1 - CY / CW) * 0.5 * Height - 0.5;
end;

function TOrbitCamera.PixelSize(const P: TVec3): Double;
var
  F, R, U: TVec3;
  D: Double;
begin
  if Ortho then
    D := Distance
  else
  begin
    Basis(F, R, U);
    D := Max((P - Eye).Dot(F), 1E-9);
  end;
  Result := 2 * D * Tan(FovY / 2) / Max(Height, 1);
end;

procedure TOrbitCamera.Orbit(DX, DY: Double);
begin
  Yaw := Yaw - DX * 0.008;
  Pitch := EnsureRange(Pitch + DY * 0.008, -Pi / 2, Pi / 2);
end;

procedure TOrbitCamera.Pan(DX, DY: Double);
var
  F, R, U: TVec3;
  S: Double;
begin
  Basis(F, R, U);
  S := PixelSize(Target);
  Target := Target - R * (DX * S) + U * (DY * S);
end;

procedure TOrbitCamera.Zoom(Factor, X, Y: Double);
var
  F, R, U, P: TVec3;
  Ray: TRay;
  T: Double;
begin
  // P: where the cursor's ray meets the plane through Target facing the
  // camera; scaling the view about P keeps P under the cursor
  Basis(F, R, U);
  Ray := RayAt(X, Y);
  T := (Target - Ray.Origin).Dot(F) / Ray.Dir.Dot(F);
  P := Ray.Origin + Ray.Dir * T;
  Target := P + (Target - P) * Factor;
  Distance := Distance * Factor;
end;

procedure TOrbitCamera.Fit(const Box: TBox);
var
  Radius, Aspect: Double;
begin
  if Box.IsEmpty then
    Exit;
  Target := Box.Center;
  Radius := Max(Box.Radius, 1E-6);
  Aspect := Max(Width, 1) / Max(Height, 1);
  // the bounding sphere fits the narrower of the two view angles
  Distance := 1.1 * Radius / Sin(Min(FovY, 2 * ArcTan(Tan(FovY / 2) * Aspect)) / 2);
  if Ortho then
    Distance := Distance * Cos(FovY / 2);
end;

procedure TOrbitCamera.SetView(V: TStandardView);
begin
  Pitch := 0;
  case V of
    svFront: Yaw := 0;
    svBack: Yaw := Pi;
    svLeft: Yaw := -Pi / 2;
    svRight: Yaw := Pi / 2;
    svTop: Pitch := Pi / 2;
    svBottom: Pitch := -Pi / 2;
    svIso:
      begin
        // from front-right-above, as a drawing's south-east isometric view
        Yaw := Pi / 4;
        Pitch := ArcTan(1 / Sqrt(2));
      end;
  end;
  if V in [svTop, svBottom] then
    Yaw := 0;
end;

end.
