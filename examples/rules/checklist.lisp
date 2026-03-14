; Default reconcile module — reproduces existing workspace semantics.
;
; Semantic notes:
; - local_checkboxes(n) returns leaf checkboxes only (enforced in the Observe layer).
; - observe_meta n "checklist-status" is the fallback when no checklist items are present.
; - Archived notes are always considered Done (expressed in DSL via relation check).

(module
  (policy
    (cycle error)
    (unknown-status todo)
    (unknown-checked false))

  (define (effective_checked c)
    (if (empty? (targets c))
        (observe_checked c)
        (all_done (map effective_status (targets c)))))

  (define (effective_status n)
    (if (eq? (observe_meta n "relation") "archived")
        done
        (if (empty? (local_checkboxes n))
            (observe_meta n "checklist-status")
            (aggregate_status (map effective_checked (local_checkboxes n)))))))
