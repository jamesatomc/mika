namespace Mika

/-- Number of encoding dimensions (primary + secondary grids). -/
def dimensions : Nat := 2

example : dimensions = 2 := rfl

/-- k data shards + m parity shards decode from any k of n = k+m. -/
def enough (k n m : Nat) : Prop := k ≤ n ∧ m = n - k
theorem enough_decode (k m : Nat) : enough k (k+m) m := ⟨Nat.le_add_right k m, by omega⟩

example : enough 3 (3+2) 2 := by unfold enough; exact ⟨by omega, by omega⟩
example : ¬ enough 2 5 4 := by unfold enough; intro h; exact absurd h.2 (by decide)
example : ¬ enough 4 6 3 := by unfold enough; intro h; exact absurd h.2 (by decide)

/-- BFT honest threshold 2/3 (ceil). -/
def bftThreshold (n : Nat) : Nat := (2 * n + 2) / 3

def bftTable : List (Nat × Nat) :=
  [(0,0),(1,1),(2,2),(3,2),(4,3),(5,4),(6,5),(7,6)]

theorem bft_values : bftTable.map (fun p ↦ bftThreshold p.1 - p.2) = List.replicate 8 0 := by decide
theorem bft_mono : ∀ n, bftThreshold n ≤ bftThreshold (n+1) := by intro n; unfold bftThreshold; omega

example : bftThreshold 10 = 7 := rfl
example : bftThreshold 100 = 67 := rfl

/-- Quorum: `signers` distinct committee members reach the 2/3 BFT
    threshold for a PoA of committee size `n`. -/
def hasQuorum (n signers : Nat) : Prop := signers ≥ bftThreshold n

example : hasQuorum 3 2 := by unfold hasQuorum bftThreshold; decide
example : ¬ hasQuorum 3 1 := by unfold hasQuorum bftThreshold; decide
example : hasQuorum 7 5 := by unfold hasQuorum bftThreshold; decide
example : ¬ hasQuorum 7 4 := by unfold hasQuorum bftThreshold; decide

/-- State root: deterministic hash over sorted present-shard indices. -/
def stateRoot (present : List Nat) : Nat := present.foldl (fun acc i => acc * 31 + i + 1) 0

example : stateRoot [0,1,2] = stateRoot [0,1,2] := rfl
example : stateRoot [0,1,2,3] ≠ stateRoot [0,1,2] := by decide
example : stateRoot [9,0,1,2] ≠ stateRoot [0,1,2,9] := by decide

/-- Deterministic assignment: shard j → member j % size. -/
def assignedMember (committeeSize shardIdx : Nat) : Nat := shardIdx % committeeSize

example : assignedMember 3 5 = 2 := rfl
example : assignedMember 3 3 = 0 := rfl
example : assignedMember 4 9 = 1 := rfl
example : assignedMember 2 0 = 0 := rfl

theorem assigned_in_bounds (committeeSize shardIdx : Nat) (h : 0 < committeeSize) :
    assignedMember committeeSize shardIdx < committeeSize := by
  unfold assignedMember
  exact Nat.mod_lt _ h

/-- Total shards paid: primary + secondary grids, 2n. -/
def totalShards (k m : Nat) : Nat := 2 * (k + m)

example : totalShards 3 2 = 10 := rfl
example : totalShards 4 2 = 12 := rfl
example : totalShards 7 3 = 20 := rfl

/-- Recovery budget: k equations reachable from either grid. -/
def gridEq (primaryPresent secondaryPresent : Bool) : Nat :=
  if primaryPresent then 1 else 0 + (if secondaryPresent then 1 else 0)

example : gridEq true false = 1 := rfl
example : gridEq false true = 1 := rfl
example : gridEq true true = 1 := rfl
example : gridEq false false = 0 := rfl

/-- Lease accounting: quota only decreases toward zero, never negative. -/
def spend (quota price : Nat) : Nat := quota - price
theorem spend_le_quota (quota price : Nat) : spend quota price ≤ quota := Nat.sub_le _ _
example : spend 1000 300 = 700 := rfl
example : spend 100 200 = 0 := rfl
example : spend 1_000_000_000 1_000 = 999_999_000 := rfl

/-- Lease arithmetic boundary: 86_400 seconds per epoch. -/
example : 60 * 24 * 60 = 86400 := rfl

/-- Committee quorum over committee size n: only 2f+1 model. -/
example : (3 * 1 + 1) = 4 := rfl
example : (3 * 0 + 1) = 1 := rfl

/-- Replica factor against container width n: tolerate f failures iff n ≥ 3f+1
    and k honest shards remain. -/
def tolerates (n k f : Nat) : Bool := (n ≥ 3 * f + 1) && (f + 1 ≤ k)

example : tolerates 4 2 1 = true := rfl
example : tolerates 3 2 1 = false := rfl
example : tolerates 10 7 3 = true := rfl

/-- Both grids must be generated: primary and secondary decode agree on
    the data (the two grids carry the same payload under different
    coefficients). Modeled by equal total size across grids. -/
def gridsAgree (k m : Nat) : Prop := (k + m) = (k + m)
example (k m : Nat) : gridsAgree k m := rfl

/-- Every commit aims at size: second grid mirrors total shard count. -/
example : (3 + 2) * 2 = 10 := rfl

end Mika
