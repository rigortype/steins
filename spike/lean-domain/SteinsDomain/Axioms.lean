import SteinsDomain.Queries
import SteinsDomain.Vectors

/-!
# The axiom ratchet

`REPORT.md` claims the proofs rest on nothing but Lean's own three axioms — no
`sorry`, no `native_decide`. A claim like that decays silently: a `sorry` left in
during a refactor still builds, and `native_decide` would quietly add
`Lean.ofReduceBool`, moving the trust boundary from the kernel to the compiler.

So the claim is a build step. Each `#guard_msgs` below fails `lake build` if the
axiom set of that theorem changes at all — which is what a stray `sorry` or a
`native_decide` does. Nothing else in this file is used by anything.

If a message here needs updating, that is the interesting event, not the chore:
say in the commit message which axiom appeared and why it is acceptable.
-/

/-! ## Soundness — the theorems the zero-FP bar rests on -/

/-- info: 'SteinsDomain.Fact.join_sound' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.join_sound

/-- info: 'SteinsDomain.Fact.fromVals_admits' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.fromVals_admits

/-- info: 'SteinsDomain.Fact.summarize_admits' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.summarize_admits

/-- info: 'SteinsDomain.Fact.join_comm' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.join_comm

/-! ## Query soundness -/

/-- info: 'SteinsDomain.Fact.truthy_yes' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.truthy_yes

/-- info: 'SteinsDomain.Fact.truthy_no' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.truthy_no

/-- info: 'SteinsDomain.Fact.satisfiesStr_yes' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.satisfiesStr_yes

/-- info: 'SteinsDomain.Fact.intIn_no' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Fact.intIn_no

/-! ## Structure -/

/-- info: 'SteinsDomain.Val.ssorted_ext' depends on axioms: [propext, Classical.choice, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Val.ssorted_ext

/-- info: 'SteinsDomain.Certainty.allOf_eq_yes_iff' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms SteinsDomain.Certainty.allOf_eq_yes_iff

/-! ## The vector model's coherence laws

These are the two obligations `Model` demands of the string classifier, so it is
worth knowing they are settled by computation and depend on nothing at all. -/

/-- info: 'SteinsDomain.Vectors.vecPreds_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms SteinsDomain.Vectors.vecPreds_closed

/-- info: 'SteinsDomain.Vectors.vecPreds_nonFalsy_iff' does not depend on any axioms -/
#guard_msgs in
#print axioms SteinsDomain.Vectors.vecPreds_nonFalsy_iff
