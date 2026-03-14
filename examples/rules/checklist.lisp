; Default reconcile module — reproduces existing workspace semantics.
;
; Semantic notes:
; - local_checkboxes(n) returns all checklist items in source order.
; - children(c) returns direct child checklist items in source order.
; - observe_meta n "checklist-status" is the fallback when no checklist items are present.
; - Archived notes are always considered Done (expressed in DSL via relation check).
; - Local non-leaf parents ignore their own checkbox marker; their children decide them.

(module
  (policy
    (cycle error)
    (unknown-status todo))

  (define (materialized_fields n)
    (list "checklist-status"))

  (define (child_status c)
    (if (empty? (children c))
        done
        (aggregate_status (map effective_checked (children c)))))

  (define (local_status c)
    (if (empty? (children c))
        (observe_checked c)
        (child_status c)))

  (define (targets_allow? c)
    (if (empty? (targets c))
        true
        (all_done (map target_status (targets c)))))

  (define (effective_checked c)
    (if (empty? (targets c))
        (local_status c)
        (if (targets_allow? c)
            (child_status c)
            todo)))

  (define (target_status n)
    (effective_meta n "checklist-status"))

  (define (effective_meta n field)
    (if (eq? field "checklist-status")
        (if (eq? (observe_meta n "relation") "archived")
            done
            (if (empty? (local_checkboxes n))
                (observe_meta n "checklist-status")
                (aggregate_status (map effective_checked (local_checkboxes n)))))
        (observe_meta n field))))
