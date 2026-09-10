-------------------------- MODULE AuthorityHandoff --------------------------
EXTENDS Naturals, Sequences, FiniteSets

(***************************************************************************)
(* Focused executable abstraction, not a model of SQLite or native APIs.   *)
(* Reads and CAS decisions are separate actions. Responses may be lost.   *)
(* One worker stands for the entire tracked descendant/native task scope. *)
(* Frames are independent identities; their SQL values are not modeled.   *)
(***************************************************************************)
CONSTANTS Dev, First, None, MaxEpoch, MaxGeneration, MaxFrames, MaxVersion,
          DrainBeforeRelease

VARIABLES remote, log, local, observed, pc, backups, discarded, badRelease
vars == <<remote, log, local, observed, pc, backups, discarded, badRelease>>

InitialRemote == [holder |-> First, epoch |-> 1, released |-> FALSE,
                  handoff |-> None, version |-> 0, seq |-> 0]
Prefix(n) == {log[i] : i \in 1..n}
Canonical == Prefix(Len(log))
Own(d, state) == state.holder = d /\ ~state.released
CAS(d) == observed[d].version = remote.version
Retired(d) == IF local[d].gate THEN local[d].retired \cup {local[d].generation}
              ELSE local[d].retired

Init ==
  /\ remote = InitialRemote
  /\ log = << >>
  /\ local = [d \in Dev |->
       [gate |-> (d = First), known |-> TRUE, resume |-> TRUE,
        generation |-> 1, retired |-> {}, worker |-> 0,
        epoch |-> 1, captured |-> 0, queued |-> {}, pending |-> {}]]
  /\ observed = [d \in Dev |-> InitialRemote]
  /\ pc = [d \in Dev |-> "idle"]
  /\ backups = [d \in Dev |-> {}]
  /\ discarded = [d \in Dev |-> {}]
  /\ badRelease = FALSE

StartWorker(d) ==
  /\ local[d].gate /\ local[d].worker = 0
  /\ local' = [local EXCEPT ![d].worker = local[d].generation]
  /\ UNCHANGED <<remote, log, observed, pc, backups, discarded, badRelease>>

Accept(d) ==
  /\ local[d].gate /\ local[d].known
  /\ local[d].worker = 0 \/ local[d].worker = local[d].generation
  /\ local[d].captured < MaxFrames
  /\ LET frame == <<d, local[d].captured + 1, local[d].epoch>> IN
       local' = [local EXCEPT ![d].queued = @ \cup {frame},
                              ![d].captured = @ + 1]
  /\ UNCHANGED <<remote, log, observed, pc, backups, discarded, badRelease>>

FinishWorker(d) ==
  /\ local[d].worker # 0
  /\ local' = [local EXCEPT ![d].worker = 0]
  /\ UNCHANGED <<remote, log, observed, pc, backups, discarded, badRelease>>

DrainQueue(d) ==
  /\ local[d].queued # {}
  /\ local' = [local EXCEPT ![d].pending = @ \cup local[d].queued,
                              ![d].queued = {}]
  /\ UNCHANGED <<remote, log, observed, pc, backups, discarded, badRelease>>

Unknown(d) ==
  /\ local[d].known \/ pc[d] # "idle"
  /\ local' = [local EXCEPT ![d].gate = FALSE, ![d].known = FALSE,
                              ![d].retired = Retired(d)]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<remote, log, observed, backups, discarded, badRelease>>

BeginRelease(d) ==
  /\ local[d].resume
  /\ local' = [local EXCEPT ![d].gate = FALSE, ![d].resume = FALSE,
                              ![d].retired = Retired(d)]
  /\ UNCHANGED <<remote, log, observed, pc, backups, discarded, badRelease>>

Read(d) ==
  /\ pc[d] = "idle"
  /\ observed' = [observed EXCEPT ![d] = remote]
  /\ pc' = [pc EXCEPT ![d] = "read"]
  \* A lost publication acknowledgement is reconciled from canonical history.
  /\ local' = [local EXCEPT ![d].pending = @ \ Canonical]
  /\ UNCHANGED <<remote, log, backups, discarded, badRelease>>

Follow(d) ==
  /\ pc[d] = "read"
  /\ ~Own(d, observed[d])
  /\ local' = [local EXCEPT ![d].gate = FALSE, ![d].known = TRUE,
                              ![d].retired = Retired(d)]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<remote, log, observed, backups, discarded, badRelease>>

Grant(d) ==
  /\ pc[d] = "read" /\ Own(d, observed[d])
  /\ observed[d].handoff = None
  /\ local[d].resume /\ ~local[d].gate
  /\ local[d].worker = 0 /\ local[d].queued = {}
  /\ \A frame \in local[d].pending : frame[3] = observed[d].epoch
  /\ local[d].generation < MaxGeneration
  /\ local' = [local EXCEPT ![d].gate = TRUE, ![d].known = TRUE,
                              ![d].generation = @ + 1,
                              ![d].epoch = observed[d].epoch]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<remote, log, observed, backups, discarded, badRelease>>

Request(d) ==
  /\ pc[d] = "read" /\ CAS(d)
  /\ ~observed[d].released /\ ~Own(d, observed[d])
  /\ observed[d].handoff = None
  /\ remote.version < MaxVersion
  /\ remote' = [remote EXCEPT !.handoff = d, !.version = @ + 1]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<log, local, observed, backups, discarded, badRelease>>

NoticeRequest(d) ==
  /\ pc[d] = "read" /\ Own(d, observed[d])
  /\ observed[d].handoff # None
  /\ local' = [local EXCEPT ![d].gate = FALSE, ![d].resume = FALSE,
                              ![d].retired = Retired(d)]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<remote, log, observed, backups, discarded, badRelease>>

Publish(d, acknowledge) ==
  /\ pc[d] = "read" /\ Own(d, observed[d]) /\ CAS(d)
  /\ remote.version < MaxVersion
  /\ \E frame \in local[d].pending :
       /\ frame[3] = observed[d].epoch
       /\ log' = Append(log, frame)
       /\ remote' = [remote EXCEPT !.seq = @ + 1, !.version = @ + 1]
       /\ local' = [local EXCEPT
           ![d].pending = IF acknowledge THEN @ \ {frame} ELSE @,
           ![d].gate = IF acknowledge THEN @ ELSE FALSE,
           ![d].known = IF acknowledge THEN @ ELSE FALSE,
           ![d].retired = IF acknowledge THEN @ ELSE Retired(d)]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<observed, backups, discarded, badRelease>>

Drained(d) == local[d].worker = 0 /\ local[d].queued = {} /\ local[d].pending = {}

Release(d, acknowledge) ==
  /\ pc[d] = "read" /\ Own(d, observed[d]) /\ CAS(d)
  /\ ~local[d].resume /\ ~local[d].gate
  /\ (~DrainBeforeRelease \/ Drained(d))
  /\ remote.version < MaxVersion
  /\ remote' = [remote EXCEPT !.released = TRUE, !.version = @ + 1]
  /\ local' = [local EXCEPT ![d].known = acknowledge]
  /\ badRelease' = (badRelease \/ ~Drained(d))
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<log, observed, backups, discarded>>

Acquire(d, force, acknowledge) ==
  /\ pc[d] = "read" /\ ~Own(d, observed[d]) /\ CAS(d)
  /\ force \/ (observed[d].released /\ observed[d].handoff \in {None, d})
  /\ ~local[d].gate /\ Drained(d)
  /\ remote.epoch < MaxEpoch /\ remote.version < MaxVersion
  /\ local[d].generation < MaxGeneration
  /\ remote' = [remote EXCEPT !.holder = d, !.released = FALSE,
                              !.handoff = None, !.epoch = @ + 1, !.version = @ + 1]
  /\ local' = [local EXCEPT ![d].gate = acknowledge, ![d].known = acknowledge,
                              ![d].resume = TRUE, ![d].generation = @ + 1,
                              ![d].epoch = remote.epoch + 1]
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<log, observed, backups, discarded, badRelease>>

Recover(d) ==
  /\ ~local[d].gate /\ local[d].worker = 0 /\ local[d].queued = {}
  /\ ~Own(d, remote) /\ local[d].pending # {}
  /\ backups' = [backups EXCEPT ![d] = @ \cup local[d].pending]
  /\ discarded' = [discarded EXCEPT ![d] = @ \cup local[d].pending]
  /\ local' = [local EXCEPT ![d].pending = {}]
  /\ UNCHANGED <<remote, log, observed, pc, badRelease>>

EndRead(d) ==
  /\ pc[d] = "read"
  /\ pc' = [pc EXCEPT ![d] = "idle"]
  /\ UNCHANGED <<remote, log, local, observed, backups, discarded, badRelease>>

Next == \E d \in Dev :
  StartWorker(d) \/ Accept(d) \/ FinishWorker(d) \/ DrainQueue(d) \/ Unknown(d)
  \/ BeginRelease(d) \/ Read(d) \/ Follow(d) \/ Grant(d) \/ Request(d)
  \/ NoticeRequest(d) \/ Recover(d) \/ EndRead(d)
  \/ (\E ack \in BOOLEAN : Publish(d, ack) \/ Release(d, ack))
  \/ (\E force, ack \in BOOLEAN : Acquire(d, force, ack))

TypeOK ==
  /\ remote.holder \in Dev /\ remote.epoch \in 1..MaxEpoch
  /\ remote.version \in 0..MaxVersion /\ remote.seq = Len(log)
  /\ \A d \in Dev :
       /\ local[d].generation \in 1..MaxGeneration
       /\ local[d].worker \in 0..MaxGeneration
       /\ local[d].captured \in 0..MaxFrames
       /\ pc[d] \in {"idle", "read"}
NoOldGeneration == \A d \in Dev : local[d].gate =>
  (local[d].generation \notin local[d].retired /\
   local[d].worker \in {0, local[d].generation})
UnknownIsClosed == \A d \in Dev : ~local[d].known => ~local[d].gate
ReleaseIntentIsClosed == \A d \in Dev : ~local[d].resume => ~local[d].gate
ReleaseWasDrained == ~badRelease
NoDuplicatePublication == Cardinality(Canonical) = Len(log)
RecoverPreserves == \A d \in Dev : discarded[d] \subseteq backups[d]
OneRemotePublisher == Cardinality({d \in Dev : Own(d, remote)}) <= 1

Spec == Init /\ [][Next]_vars
=============================================================================
