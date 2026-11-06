// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf.Math - the little vector and matrix math the viewer needs.

  TMat4 uses the convention of dwg2gltf.dll and glTF: 16 values,
  column-major, m[c * 4 + r] is row r of column c, points are column vectors
  (p' = M * p), the translation is m[12..14]. Uploaded unchanged into an
  HLSL constant buffer (whose default packing is column-major) it is the
  float4x4 for mul(M, float4(p, 1)). The same memory is the row-vector
  matrix of DirectXMath, so either way of thinking works.

  Camera matrices are right-handed like the scene (glTF: Y up, the camera
  looks along -Z) with Direct3D's clip depth 0..1, so no mirroring is
  needed; the rasterizer treats counter-clockwise triangles as front faces.

  Doubles on the CPU, floats only where data goes to the GPU. }
unit Dwg2Gltf.Math;

interface

type
  TVec3 = record
    X, Y, Z: Double;
    class function Create(AX, AY, AZ: Double): TVec3; static; inline;
    class operator Add(const A, B: TVec3): TVec3; inline;
    class operator Subtract(const A, B: TVec3): TVec3; inline;
    class operator Multiply(const A: TVec3; S: Double): TVec3; inline;
    class operator Multiply(S: Double; const A: TVec3): TVec3; inline;
    class operator Negative(const A: TVec3): TVec3; inline;
    function Dot(const B: TVec3): Double; inline;
    function Cross(const B: TVec3): TVec3; inline;
    function Length: Double; inline;
    function Normalized: TVec3;
  end;

  TMat4 = record
    M: array[0..15] of Double;
    function Get(Row, Col: Integer): Double; inline;
    procedure SetAt(Row, Col: Integer; Value: Double); inline;
    class function Identity: TMat4; static;
    class function FromFloats(const F: array of Single): TMat4; static;
    class function FromDoubles(const D: array of Double): TMat4; static;
    { A * B: apply B first, then A. }
    class operator Multiply(const A, B: TMat4): TMat4;
    function TransformPoint(const P: TVec3): TVec3;
    function TransformVector(const V: TVec3): TVec3;
    { Clip coordinates (x, y, z, w) of a point. }
    procedure TransformClip(const P: TVec3; out X, Y, Z, W: Double);
    function Inverse: TMat4;
    { Determinant of the upper 3x3: negative for a mirroring transform. }
    function Det3: Double;
    { Inverse transpose of the upper 3x3 (in the upper 3x3 of the result):
      transforms normals of a non-uniformly scaled or mirrored mesh. }
    function NormalMatrix: TMat4;
  end;

  TMat4F = array[0..15] of Single;

  TBox = record
    Min, Max: TVec3;
    procedure Clear;
    procedure Include(const P: TVec3);
    function IsEmpty: Boolean;
    function Center: TVec3;
    function Radius: Double;
    { The box of the 8 corners of this box transformed by M. }
    function Transformed(const M: TMat4): TBox;
  end;

  TRay = record
    Origin, Dir: TVec3; // Dir is unit length
  end;

function ToFloat(const M: TMat4): TMat4F;
function LookAtRH(const Eye, Target, Up: TVec3): TMat4;
{ Right-handed perspective projection, clip depth 0..1. }
function PerspectiveRH(FovY, Aspect, ZNear, ZFar: Double): TMat4;
{ Right-handed orthographic projection, clip depth 0..1. }
function OrthoRH(Width, Height, ZNear, ZFar: Double): TMat4;
{ Ray against an axis-aligned box: the entry and exit distances, False when
  it misses. TMax limits the search (the nearest hit so far). }
function RayBox(const R: TRay; const BoxMin, BoxMax: TVec3; TMax: Double;
  out T0, T1: Double): Boolean;
{ Moeller-Trumbore, both faces: True with the distance T along R. }
function RayTriangle(const O, D, A, B, C: TVec3; out T: Double): Boolean;
{ Closest points of a ray and a segment A..B: T along the ray, S in 0..1
  along the segment. }
procedure RaySegment(const R: TRay; const A, B: TVec3; out T, S: Double);

implementation

uses
  System.Math;

{ TVec3 }

class function TVec3.Create(AX, AY, AZ: Double): TVec3;
begin
  Result.X := AX;
  Result.Y := AY;
  Result.Z := AZ;
end;

class operator TVec3.Add(const A, B: TVec3): TVec3;
begin
  Result := TVec3.Create(A.X + B.X, A.Y + B.Y, A.Z + B.Z);
end;

class operator TVec3.Subtract(const A, B: TVec3): TVec3;
begin
  Result := TVec3.Create(A.X - B.X, A.Y - B.Y, A.Z - B.Z);
end;

class operator TVec3.Multiply(const A: TVec3; S: Double): TVec3;
begin
  Result := TVec3.Create(A.X * S, A.Y * S, A.Z * S);
end;

class operator TVec3.Multiply(S: Double; const A: TVec3): TVec3;
begin
  Result := TVec3.Create(A.X * S, A.Y * S, A.Z * S);
end;

class operator TVec3.Negative(const A: TVec3): TVec3;
begin
  Result := TVec3.Create(-A.X, -A.Y, -A.Z);
end;

function TVec3.Dot(const B: TVec3): Double;
begin
  Result := X * B.X + Y * B.Y + Z * B.Z;
end;

function TVec3.Cross(const B: TVec3): TVec3;
begin
  Result := TVec3.Create(Y * B.Z - Z * B.Y, Z * B.X - X * B.Z, X * B.Y - Y * B.X);
end;

function TVec3.Length: Double;
begin
  Result := Sqrt(X * X + Y * Y + Z * Z);
end;

function TVec3.Normalized: TVec3;
var
  L: Double;
begin
  L := Length;
  if L > 0 then
    Result := Self * (1 / L)
  else
    Result := Self;
end;

{ TMat4 }

function TMat4.Get(Row, Col: Integer): Double;
begin
  Result := M[Col * 4 + Row];
end;

procedure TMat4.SetAt(Row, Col: Integer; Value: Double);
begin
  M[Col * 4 + Row] := Value;
end;

class function TMat4.Identity: TMat4;
begin
  FillChar(Result, SizeOf(Result), 0);
  Result.M[0] := 1;
  Result.M[5] := 1;
  Result.M[10] := 1;
  Result.M[15] := 1;
end;

class function TMat4.FromFloats(const F: array of Single): TMat4;
var
  I: Integer;
begin
  for I := 0 to 15 do
    Result.M[I] := F[I];
end;

class function TMat4.FromDoubles(const D: array of Double): TMat4;
var
  I: Integer;
begin
  for I := 0 to 15 do
    Result.M[I] := D[I];
end;

class operator TMat4.Multiply(const A, B: TMat4): TMat4;
var
  R, C, K: Integer;
  S: Double;
begin
  for C := 0 to 3 do
    for R := 0 to 3 do
    begin
      S := 0;
      for K := 0 to 3 do
        S := S + A.M[K * 4 + R] * B.M[C * 4 + K];
      Result.M[C * 4 + R] := S;
    end;
end;

function TMat4.TransformPoint(const P: TVec3): TVec3;
begin
  Result.X := M[0] * P.X + M[4] * P.Y + M[8] * P.Z + M[12];
  Result.Y := M[1] * P.X + M[5] * P.Y + M[9] * P.Z + M[13];
  Result.Z := M[2] * P.X + M[6] * P.Y + M[10] * P.Z + M[14];
end;

function TMat4.TransformVector(const V: TVec3): TVec3;
begin
  Result.X := M[0] * V.X + M[4] * V.Y + M[8] * V.Z;
  Result.Y := M[1] * V.X + M[5] * V.Y + M[9] * V.Z;
  Result.Z := M[2] * V.X + M[6] * V.Y + M[10] * V.Z;
end;

procedure TMat4.TransformClip(const P: TVec3; out X, Y, Z, W: Double);
begin
  X := M[0] * P.X + M[4] * P.Y + M[8] * P.Z + M[12];
  Y := M[1] * P.X + M[5] * P.Y + M[9] * P.Z + M[13];
  Z := M[2] * P.X + M[6] * P.Y + M[10] * P.Z + M[14];
  W := M[3] * P.X + M[7] * P.Y + M[11] * P.Z + M[15];
end;

function TMat4.Inverse: TMat4;
var
  Inv: array[0..15] of Double;
  Det: Double;
  I: Integer;
begin
  // cofactor expansion (the layout does not matter: inverse of the
  // transpose is the transpose of the inverse)
  Inv[0] := M[5] * M[10] * M[15] - M[5] * M[11] * M[14] - M[9] * M[6] * M[15] +
    M[9] * M[7] * M[14] + M[13] * M[6] * M[11] - M[13] * M[7] * M[10];
  Inv[4] := -M[4] * M[10] * M[15] + M[4] * M[11] * M[14] + M[8] * M[6] * M[15] -
    M[8] * M[7] * M[14] - M[12] * M[6] * M[11] + M[12] * M[7] * M[10];
  Inv[8] := M[4] * M[9] * M[15] - M[4] * M[11] * M[13] - M[8] * M[5] * M[15] +
    M[8] * M[7] * M[13] + M[12] * M[5] * M[11] - M[12] * M[7] * M[9];
  Inv[12] := -M[4] * M[9] * M[14] + M[4] * M[10] * M[13] + M[8] * M[5] * M[14] -
    M[8] * M[6] * M[13] - M[12] * M[5] * M[10] + M[12] * M[6] * M[9];
  Inv[1] := -M[1] * M[10] * M[15] + M[1] * M[11] * M[14] + M[9] * M[2] * M[15] -
    M[9] * M[3] * M[14] - M[13] * M[2] * M[11] + M[13] * M[3] * M[10];
  Inv[5] := M[0] * M[10] * M[15] - M[0] * M[11] * M[14] - M[8] * M[2] * M[15] +
    M[8] * M[3] * M[14] + M[12] * M[2] * M[11] - M[12] * M[3] * M[10];
  Inv[9] := -M[0] * M[9] * M[15] + M[0] * M[11] * M[13] + M[8] * M[1] * M[15] -
    M[8] * M[3] * M[13] - M[12] * M[1] * M[11] + M[12] * M[3] * M[9];
  Inv[13] := M[0] * M[9] * M[14] - M[0] * M[10] * M[13] - M[8] * M[1] * M[14] +
    M[8] * M[2] * M[13] + M[12] * M[1] * M[10] - M[12] * M[2] * M[9];
  Inv[2] := M[1] * M[6] * M[15] - M[1] * M[7] * M[14] - M[5] * M[2] * M[15] +
    M[5] * M[3] * M[14] + M[13] * M[2] * M[7] - M[13] * M[3] * M[6];
  Inv[6] := -M[0] * M[6] * M[15] + M[0] * M[7] * M[14] + M[4] * M[2] * M[15] -
    M[4] * M[3] * M[14] - M[12] * M[2] * M[7] + M[12] * M[3] * M[6];
  Inv[10] := M[0] * M[5] * M[15] - M[0] * M[7] * M[13] - M[4] * M[1] * M[15] +
    M[4] * M[3] * M[13] + M[12] * M[1] * M[7] - M[12] * M[3] * M[5];
  Inv[14] := -M[0] * M[5] * M[14] + M[0] * M[6] * M[13] + M[4] * M[1] * M[14] -
    M[4] * M[2] * M[13] - M[12] * M[1] * M[6] + M[12] * M[2] * M[5];
  Inv[3] := -M[1] * M[6] * M[11] + M[1] * M[7] * M[10] + M[5] * M[2] * M[11] -
    M[5] * M[3] * M[10] - M[9] * M[2] * M[7] + M[9] * M[3] * M[6];
  Inv[7] := M[0] * M[6] * M[11] - M[0] * M[7] * M[10] - M[4] * M[2] * M[11] +
    M[4] * M[3] * M[10] + M[8] * M[2] * M[7] - M[8] * M[3] * M[6];
  Inv[11] := -M[0] * M[5] * M[11] + M[0] * M[7] * M[9] + M[4] * M[1] * M[11] -
    M[4] * M[3] * M[9] - M[8] * M[1] * M[7] + M[8] * M[3] * M[5];
  Inv[15] := M[0] * M[5] * M[10] - M[0] * M[6] * M[9] - M[4] * M[1] * M[10] +
    M[4] * M[2] * M[9] + M[8] * M[1] * M[6] - M[8] * M[2] * M[5];
  Det := M[0] * Inv[0] + M[1] * Inv[4] + M[2] * Inv[8] + M[3] * Inv[12];
  if Det = 0 then
    Exit(TMat4.Identity);
  Det := 1 / Det;
  for I := 0 to 15 do
    Result.M[I] := Inv[I] * Det;
end;

function TMat4.Det3: Double;
begin
  Result := Get(0, 0) * (Get(1, 1) * Get(2, 2) - Get(1, 2) * Get(2, 1)) -
    Get(0, 1) * (Get(1, 0) * Get(2, 2) - Get(1, 2) * Get(2, 0)) +
    Get(0, 2) * (Get(1, 0) * Get(2, 1) - Get(1, 1) * Get(2, 0));
end;

function TMat4.NormalMatrix: TMat4;
var
  D: Double;
  R, C: Integer;
begin
  // inverse transpose = cofactor matrix / determinant. Dividing by |det|
  // instead negates the normals of a mirrored instance, whose triangles
  // also turn clockwise on screen: the shader's flip of back-face normals
  // (SV_IsFrontFace) then turns them the right way again.
  Result := TMat4.Identity;
  for R := 0 to 2 do
    for C := 0 to 2 do
      Result.SetAt(R, C,
        Get((R + 1) mod 3, (C + 1) mod 3) * Get((R + 2) mod 3, (C + 2) mod 3) -
        Get((R + 1) mod 3, (C + 2) mod 3) * Get((R + 2) mod 3, (C + 1) mod 3));
  D := Abs(Det3);
  if D > 0 then
    for R := 0 to 2 do
      for C := 0 to 2 do
        Result.SetAt(R, C, Result.Get(R, C) / D);
end;

function ToFloat(const M: TMat4): TMat4F;
var
  I: Integer;
begin
  for I := 0 to 15 do
    Result[I] := M.M[I];
end;

{ TBox }

procedure TBox.Clear;
begin
  Min := TVec3.Create(MaxDouble, MaxDouble, MaxDouble);
  Max := TVec3.Create(-MaxDouble, -MaxDouble, -MaxDouble);
end;

procedure TBox.Include(const P: TVec3);
begin
  if P.X < Min.X then Min.X := P.X;
  if P.Y < Min.Y then Min.Y := P.Y;
  if P.Z < Min.Z then Min.Z := P.Z;
  if P.X > Max.X then Max.X := P.X;
  if P.Y > Max.Y then Max.Y := P.Y;
  if P.Z > Max.Z then Max.Z := P.Z;
end;

function TBox.IsEmpty: Boolean;
begin
  Result := Min.X > Max.X;
end;

function TBox.Center: TVec3;
begin
  Result := (Min + Max) * 0.5;
end;

function TBox.Radius: Double;
begin
  Result := (Max - Min).Length * 0.5;
end;

function TBox.Transformed(const M: TMat4): TBox;
var
  I: Integer;
  P: TVec3;
begin
  Result.Clear;
  if IsEmpty then
    Exit;
  for I := 0 to 7 do
  begin
    if I and 1 = 0 then P.X := Min.X else P.X := Max.X;
    if I and 2 = 0 then P.Y := Min.Y else P.Y := Max.Y;
    if I and 4 = 0 then P.Z := Min.Z else P.Z := Max.Z;
    Result.Include(M.TransformPoint(P));
  end;
end;

{ camera matrices }

function LookAtRH(const Eye, Target, Up: TVec3): TMat4;
var
  F, S, U: TVec3;
begin
  F := (Target - Eye).Normalized;
  S := F.Cross(Up).Normalized;
  U := S.Cross(F);
  Result := TMat4.Identity;
  Result.SetAt(0, 0, S.X); Result.SetAt(0, 1, S.Y); Result.SetAt(0, 2, S.Z);
  Result.SetAt(1, 0, U.X); Result.SetAt(1, 1, U.Y); Result.SetAt(1, 2, U.Z);
  Result.SetAt(2, 0, -F.X); Result.SetAt(2, 1, -F.Y); Result.SetAt(2, 2, -F.Z);
  Result.SetAt(0, 3, -S.Dot(Eye));
  Result.SetAt(1, 3, -U.Dot(Eye));
  Result.SetAt(2, 3, F.Dot(Eye));
end;

function PerspectiveRH(FovY, Aspect, ZNear, ZFar: Double): TMat4;
var
  YS: Double;
begin
  YS := 1 / Tan(FovY / 2);
  FillChar(Result, SizeOf(Result), 0);
  Result.SetAt(0, 0, YS / Aspect);
  Result.SetAt(1, 1, YS);
  Result.SetAt(2, 2, ZFar / (ZNear - ZFar));
  Result.SetAt(2, 3, ZNear * ZFar / (ZNear - ZFar));
  Result.SetAt(3, 2, -1);
end;

function OrthoRH(Width, Height, ZNear, ZFar: Double): TMat4;
begin
  Result := TMat4.Identity;
  Result.SetAt(0, 0, 2 / Width);
  Result.SetAt(1, 1, 2 / Height);
  Result.SetAt(2, 2, 1 / (ZNear - ZFar));
  Result.SetAt(2, 3, ZNear / (ZNear - ZFar));
end;

{ ray tests }

function RayBox(const R: TRay; const BoxMin, BoxMax: TVec3; TMax: Double;
  out T0, T1: Double): Boolean;

  function Slab(O, D, Lo, Hi: Double): Boolean;
  var
    A, B, Inv: Double;
  begin
    if Abs(D) < 1E-300 then
      Exit((O >= Lo) and (O <= Hi));
    Inv := 1 / D;
    A := (Lo - O) * Inv;
    B := (Hi - O) * Inv;
    if A > B then
    begin
      Inv := A; A := B; B := Inv;
    end;
    if A > T0 then T0 := A;
    if B < T1 then T1 := B;
    Result := T0 <= T1;
  end;

begin
  T0 := 0;
  T1 := TMax;
  Result := Slab(R.Origin.X, R.Dir.X, BoxMin.X, BoxMax.X) and
    Slab(R.Origin.Y, R.Dir.Y, BoxMin.Y, BoxMax.Y) and
    Slab(R.Origin.Z, R.Dir.Z, BoxMin.Z, BoxMax.Z);
end;

function RayTriangle(const O, D, A, B, C: TVec3; out T: Double): Boolean;
var
  E1, E2, P, Q, S: TVec3;
  Det, InvDet, U, V: Double;
begin
  Result := False;
  E1 := B - A;
  E2 := C - A;
  P := D.Cross(E2);
  Det := E1.Dot(P);
  if Abs(Det) < 1E-30 then
    Exit;
  InvDet := 1 / Det;
  S := O - A;
  U := S.Dot(P) * InvDet;
  if (U < 0) or (U > 1) then
    Exit;
  Q := S.Cross(E1);
  V := D.Dot(Q) * InvDet;
  if (V < 0) or (U + V > 1) then
    Exit;
  T := E2.Dot(Q) * InvDet;
  Result := T > 0;
end;

procedure RaySegment(const R: TRay; const A, B: TVec3; out T, S: Double);
var
  U, W: TVec3;
  BB, UD, UW, DW, Den: Double;
begin
  // minimize |O + t D - (A + s U)| with |D| = 1
  U := B - A;
  W := R.Origin - A;
  BB := U.Dot(U);
  UD := U.Dot(R.Dir);
  UW := U.Dot(W);
  DW := R.Dir.Dot(W);
  Den := BB - UD * UD;
  if (BB <= 0) or (Den <= 1E-12 * BB) then
    S := 0 // parallel or a point: any s, take A
  else
    S := (UW - UD * DW) / Den;
  S := EnsureRange(S, 0, 1);
  T := (A + U * S - R.Origin).Dot(R.Dir);
end;

end.
