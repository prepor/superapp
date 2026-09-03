------------------------------- MODULE Lease -------------------------------
(***************************************************************************)
(* Device sync: two devices, one store, and one writer at a time.          *)
(*                                                                         *)
(* The bucket holds one `state` object — epoch, holder, released, seq —    *)
(* advanced ONLY by compare-and-swap on its version (the HTTP ETag).       *)
(* Batches are immutable create-only objects, so the model keeps just the  *)
(* published history (`log`) and each device's local view.                *)
(*                                                                         *)
(* Every round trip in src/repl.rs is a READ followed by a DECIDE/CAS.     *)
(* Those are modelled as two atomic steps — the read snapshots the state   *)
(* and its version into `obs[d]`, the decide acts on that snapshot and its *)
(* CAS succeeds only if the version is still current — so every            *)
(* interleaving between one device's read and another's write is explored.*)
(*                                                                         *)
(* A "frame" is one captured local write, tagged with the epoch the device *)
(* believed it held when it wrote. That tag is what lets the model ask the *)
(* question the code cannot: does a write made under a superseded lease    *)
(* ever enter the canonical history?                                       *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS Dev,       \* the devices
          MaxEpoch,  \* how many lease epochs to explore
          MaxCap,    \* how many writes each device may capture
          None,      \* "no holder"
          Discard    \* the proposed fix: drop frames captured under an older
                     \* epoch whenever a newer one is adopted (see NoStaleWrite)

VARIABLES
  bucket,  \* the `state` object: exists, epoch, holder, released, seq, ver
  log,     \* the canonical history: every published frame, in order
  holders, \* holders[e]: the device that held epoch e (a history)
  dev,     \* per device: local lease bookkeeping, the write gate, the role
  obs,     \* per device: the state it last read, with its version
  pc       \* per device: which round trip is between its read and decide

vars == <<bucket, log, holders, dev, obs, pc>>

Roles == {"detached", "holder", "free", "follower", "stranded", "offline"}

NoBucket == [exists |-> FALSE, epoch |-> 0, holder |-> None,
             released |-> FALSE, seq |-> 0, ver |-> 0]

TypeOK ==
  /\ bucket.epoch \in 0..MaxEpoch
  /\ bucket.holder \in Dev \cup {None}
  /\ \A d \in Dev : dev[d].role \in Roles /\ pc[d] \in {"idle", "poll", "acq", "rel"}

\* A captured write, and the same write once published.
Frame(d, e) == [dev |-> d, cap |-> e]
Published(fr, e) ==
  [i \in 1..Len(fr) |-> [dev |-> fr[i].dev, cap |-> fr[i].cap, pub |-> e]]

Init ==
  /\ bucket = NoBucket
  /\ log = << >>
  /\ holders = << >>
  /\ dev = [d \in Dev |-> [epoch |-> 0, holding |-> FALSE, mat |-> 0,
                           pending |-> << >>, writable |-> TRUE,
                           role |-> "detached", caps |-> 0]]
  /\ obs = [d \in Dev |-> NoBucket]
  /\ pc = [d \in Dev |-> "idle"]

-----------------------------------------------------------------------------
(* A local write. Only an open gate admits one; the frame remembers the    *)
(* epoch the device believed it held.                                      *)
Capture(d) ==
  /\ pc[d] = "idle"
  /\ dev[d].writable
  /\ dev[d].caps < MaxCap
  /\ dev' = [dev EXCEPT ![d].pending = Append(@, Frame(d, dev[d].epoch)),
                        ![d].caps = @ + 1]
  /\ UNCHANGED <<bucket, log, holders, obs, pc>>

(* The read half of any round trip: snapshot the state and its version.   *)
Read(d, next) ==
  /\ pc[d] = "idle"
  /\ obs' = [obs EXCEPT ![d] = bucket]
  /\ pc' = [pc EXCEPT ![d] = next]
  /\ UNCHANGED <<bucket, log, holders, dev>>

(* The role a device falls back to when the bucket cannot be reached:     *)
(* a holder keeps writing, a never-joined device stays local, a follower  *)
(* stays locked (repl.rs `offline_role`).                                  *)
OfflineRole(d) ==
  IF dev[d].holding THEN "holder"
  ELSE IF dev[d].epoch = 0 THEN "detached"
  ELSE "offline"

SetOffline(d) ==
  LET r == OfflineRole(d) IN
  dev' = [dev EXCEPT ![d].role = r, ![d].writable = (r \in {"holder", "detached"})]

Offline(d) ==   \* a pass whose read fails
  /\ pc[d] = "idle"
  /\ SetOffline(d)
  /\ UNCHANGED <<bucket, log, holders, obs, pc>>

NetFail(d) ==   \* a request after the read fails: the pass ends offline
  /\ pc[d] /= "idle"
  /\ SetOffline(d)
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<bucket, log, holders, obs>>

-----------------------------------------------------------------------------
(* Poll (`poll_inner`), decided against what was read.                     *)

\* No lineage yet: try to become canonical, create-only. Losing the race
\* just means polling again.
Bootstrap(d) ==
  /\ pc[d] = "poll"
  /\ ~obs[d].exists
  /\ IF ~bucket.exists
       THEN /\ bucket' = [exists |-> TRUE, epoch |-> 1, holder |-> d,
                          released |-> FALSE, seq |-> 0, ver |-> 1]
            /\ dev' = [dev EXCEPT ![d].epoch = 1, ![d].holding = TRUE,
                                  ![d].pending = << >>,   \* folded into the snapshot
                                  ![d].writable = TRUE, ![d].role = "holder"]
            /\ holders' = <<d>>
       ELSE UNCHANGED <<bucket, dev, holders>>
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<log, obs>>

WeHold(d) == obs[d].exists /\ obs[d].holder = d /\ ~obs[d].released

\* We hold: adopt the epoch and publish what we captured. The CAS may lose
\* (the lease moved under us): the pass still answers Holder — the next
\* pass re-reads and re-roles.
PollHold(d) ==
  /\ pc[d] = "poll"
  /\ WeHold(d)
  /\ LET n == Len(dev[d].pending)
         won == bucket.ver = obs[d].ver
     IN IF n > 0 /\ won
          THEN /\ bucket' = [bucket EXCEPT !.seq = @ + n, !.ver = @ + 1]
               /\ log' = log \o Published(dev[d].pending, obs[d].epoch)
               /\ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch, ![d].holding = TRUE,
                                     ![d].pending = << >>, ![d].mat = obs[d].seq + n,
                                     ![d].writable = TRUE, ![d].role = "holder"]
          ELSE /\ UNCHANGED <<bucket, log>>
               /\ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch, ![d].holding = TRUE,
                                     ![d].writable = TRUE, ![d].role = "holder"]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<holders, obs>>

\* We thought we held, the lineage moved past us, AND we hold writes that
\* never reached the history. Only that last clause is divergence — a holder
\* overridden with nothing unpublished follows cleanly instead of stranding.
Superseded(d) ==
  /\ dev[d].epoch > 0 /\ dev[d].holding
  /\ obs[d].epoch > dev[d].epoch /\ ~obs[d].released
  /\ Len(dev[d].pending) > 0

\* Overridden while holding: read-only, recovery is manual. Note `holding`
\* is deliberately left set — it is what makes the stranding detectable.
PollStranded(d) ==
  /\ pc[d] = "poll"
  /\ obs[d].exists /\ ~WeHold(d)
  /\ Superseded(d)
  /\ dev' = [dev EXCEPT ![d].role = "stranded", ![d].writable = FALSE]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<bucket, log, holders, obs>>

\* A follower, or the lease is free: a never-joined device first installs
\* the snapshot (clearing its own log); then adopt the epoch, drop
\* `holding`, and materialize up to the head. Materializing over local
\* writes made under an older lease can conflict — then the pass errors and
\* ends offline.
PollFollow(d) ==
  /\ pc[d] = "poll"
  /\ obs[d].exists /\ ~WeHold(d)
  /\ ~Superseded(d)
  /\ LET stale == Discard /\ dev[d].epoch < obs[d].epoch
         pend == IF dev[d].epoch = 0 \/ stale THEN << >> ELSE dev[d].pending
         role == IF obs[d].released THEN "free" ELSE "follower"
     IN \/ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch, ![d].holding = FALSE,
                              ![d].pending = pend, ![d].mat = obs[d].seq,
                              ![d].writable = FALSE, ![d].role = role]
        \/ /\ pend /= << >>
           /\ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch, ![d].holding = FALSE,
                                 ![d].pending = pend,
                                 ![d].writable = FALSE, ![d].role = "offline"]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<bucket, log, holders, obs>>

-----------------------------------------------------------------------------
(* Acquire (`acquire`; `override_lease` is the same operation): catch up to *)
(* the head, then CAS the lease to us at epoch + 1. If catching up over   *)
(* local divergent writes conflicts, the store is reset to the snapshot   *)
(* and replayed — the divergent writes are discarded. If it does NOT      *)
(* conflict, they survive as pending frames of the new holder.            *)
AcqDecide(d) ==
  /\ pc[d] = "acq"
  /\ obs[d].exists
  /\ obs[d].epoch < MaxEpoch
  /\ LET pend0 == IF dev[d].epoch = 0 THEN << >> ELSE dev[d].pending
         won == bucket.ver = obs[d].ver
     IN \E conflict \in BOOLEAN :
          /\ conflict => pend0 /= << >>
          /\ (Discard /\ pend0 /= << >> /\ dev[d].epoch < obs[d].epoch) => conflict
          /\ LET pend == IF conflict THEN << >> ELSE pend0 IN
             IF won
               THEN /\ bucket' = [bucket EXCEPT !.holder = d, !.released = FALSE,
                                                !.epoch = @ + 1, !.ver = @ + 1]
                    /\ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch + 1,
                                          ![d].holding = TRUE, ![d].pending = pend,
                                          ![d].mat = obs[d].seq,
                                          ![d].writable = TRUE, ![d].role = "holder"]
                    /\ holders' = Append(holders, d)
               ELSE /\ UNCHANGED <<bucket, holders>>   \* version moved: re-read and retry
                    /\ dev' = [dev EXCEPT ![d].pending = pend, ![d].mat = obs[d].seq]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<log, obs>>

(* Release (`release`): publish what we captured, then CAS `released`.    *)
(* Not ours to release (overridden meanwhile): just close the gate.       *)
RelDecide(d) ==
  /\ pc[d] = "rel"
  /\ obs[d].exists
  /\ IF obs[d].holder /= d \/ obs[d].released
       THEN /\ dev' = [dev EXCEPT ![d].writable = FALSE, ![d].role = "free"]
            /\ UNCHANGED <<bucket, log>>
       ELSE LET n == Len(dev[d].pending) IN
            IF bucket.ver = obs[d].ver
              THEN /\ bucket' = [bucket EXCEPT !.seq = @ + n, !.released = TRUE,
                                               !.ver = @ + 1]
                   /\ log' = log \o Published(dev[d].pending, obs[d].epoch)
                   /\ dev' = [dev EXCEPT ![d].epoch = obs[d].epoch, ![d].holding = FALSE,
                                         ![d].pending = << >>, ![d].mat = obs[d].seq + n,
                                         ![d].writable = FALSE, ![d].role = "free"]
              ELSE UNCHANGED <<bucket, log, dev>>   \* version moved: retry
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<holders, obs>>

\* acquire/release with no lineage yet: the request errors out.
NoLineage(d) ==
  /\ pc[d] \in {"acq", "rel"}
  /\ ~obs[d].exists
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<bucket, log, holders, dev, obs>>

-----------------------------------------------------------------------------
Next ==
  \E d \in Dev :
    \/ Capture(d)
    \/ Read(d, "poll") \/ Read(d, "acq") \/ Read(d, "rel")
    \/ Offline(d) \/ NetFail(d)
    \/ Bootstrap(d) \/ PollHold(d) \/ PollStranded(d) \/ PollFollow(d)
    \/ AcqDecide(d) \/ RelDecide(d) \/ NoLineage(d)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties.                                                             *)

\* The design's promise: at most one writer of the CURRENT lineage.
CurrentWriters ==
  {d \in Dev : dev[d].writable /\ dev[d].holding /\ dev[d].epoch = bucket.epoch}
OneCurrentWriter == Cardinality(CurrentWriters) <= 1

\* Stronger — at most one joined device writable at all. Expected to FAIL,
\* by design: a superseded holder keeps writing until its next pass.
JoinedWriters == {d \in Dev : dev[d].writable /\ dev[d].epoch > 0}
OneWriter == Cardinality(JoinedWriters) <= 1

\* The history is published under non-decreasing epochs — the CAS fences a
\* superseded holder out of publishing.
LogEpochsMonotone ==
  \A i, j \in 1..Len(log) : i < j => log[i].pub <= log[j].pub

\* Every published frame was captured under the epoch it was published in.
\* Too strict on its own: a device re-acquiring its own lease republishes
\* harmlessly. See NoStaleWrite.
NoStalePublish == \A i \in 1..Len(log) : log[i].cap = log[i].pub

\* The hazard that matters: a write captured under epoch C and published
\* under epoch P, where ANOTHER device held some epoch in between — a
\* write made against a state the history has since moved past, landing
\* after (and possibly clobbering) newer writes.
NoStaleWrite ==
  \A i \in 1..Len(log) :
    \A e \in (log[i].cap + 1)..log[i].pub : holders[e] = log[i].dev

\* Nobody materializes past the head.
MatBounded == \A d \in Dev : dev[d].mat <= bucket.seq

\* A device the bucket has moved past is read-only once it has polled:
\* `holding` may stay set (it is how stranding is detected), but the gate
\* is closed.
StrandedIsReadOnly ==
  \A d \in Dev : dev[d].role = "stranded" => ~dev[d].writable

StateBound == bucket.ver <= 16 /\ Len(log) <= 4

=============================================================================
