// Trusted Zio sources. Embedded as strings so the bridge can register
// them with the WASM host's in-memory filesystem (add_source). The
// sources are the exact same files checked into apps/grove/; the
// parent (or a build step) regenerates this file when the sources change.
/* eslint-disable */
export const CONTROLLER_PATH = '/zio/apps/grove/browser-controller.zio';
export const CONTROLLER_SOURCE = `;; Browser business runs in the persistent, fail-closed WASM Zio session.
;; AST startup is explicit until the installed portable controller is compiled.
;; All bridge effects are data. No DOM, ambient filesystem or product decisions.
;; The controller emits a command list; the JS bridge executes it.
;;
;; Command primitives (the bridge surface):
;;   {:op "text"       :target "#id" :text "..." :class "ok|error"}
;;   {:op "value"      :target "#id" :value "..."}
;;   {:op "replace"    :target "#id tbody|..." :children [{:tag ...} ...]}
;;   {:op "attribute"  :target "#id" :name "aria-busy" :value "true|false|string"}
;;   {:op "focus"      :target "#id"}
;;   {:op "canvas"     :target "#id" :width N :height N :pixels [u8 ...]}
;;   {:op "svg"        :target "#id" :width N :height N :markup "<svg ...>...</svg>"}
;;   {:op "http"       :authority "same-origin" :method ... :path [...] :query {}
;;                    :headers {} :body {} :context {:purpose :output}}
;;   {:op "timer"      :key "trace" :milliseconds N}
;;   {:op "timer-cancel" :key "trace"}
;;   {:op "storage-get" :key "..." :context {:action "..."}}
;;   {:op "storage-set" :key "..." :value "..."}
;;   {:op "confirm"    :message "..." :context {:action "approve" :body {...}}}
;;   {:op "log"        :level "info|warn|error" :text "..."}
;;
;; The Zio controller never touches DOM, fetches the network, or stores the
;; bearer token. The bearer token is read on demand inside :op "http" only.

(defn grove--browser-get [m key fallback]
  (get m (keyword key) (get m key fallback)))
(defn grove--browser-field [event id]
  (str-trim (grove--browser-get (grove--browser-get event "fields" {}) id "")))
(defn grove--browser-csv [text]
  (into [] (filter (fn [s] (not (= s ""))) (map str-trim (str-split "," text)))))
(defn grove--browser-number [text]
  (let [n (json-parse text)] (if (number? n) n (error "Enter a finite JSON number"))))
(defn grove--browser-natural [text]
  (let [n (grove--browser-number text)]
    (if (and (>= n 0) (= (mod n 1) 0)) n (error "Enter a nonnegative integer"))))
(defn grove--browser-nullable [text] (if (= text "") nil text))
(defn grove--browser-parse-loss [detail]
  ;; "loss=0.321 step=42" → 0.321 ; anything else → nil
  (let [i (str-index detail "loss=")]
    (if (nil? i) nil
      (let [rest (slice detail (+ i 5) (count detail))]
        (let [j (loop [k 0] (if (= k (count rest)) k
          (let [c (slice rest k (+ k 1))]
            (if (or (= c " ") (= c ",") (= c ";") (= c "\n") (= c "\t")) k
              (recur (+ k 1))))))]
          (grove--browser-number (slice rest 0 j)))))))
(defn grove--browser-node [tag text attrs children]
  {:tag tag :text text :attrs attrs :children (into [] children)})
(defn grove--browser-text [id text bad]
  {:op "text" :target (str "#" id) :text text :class (if bad "error" "ok")})
(defn grove--browser-value [id value] {:op "value" :target (str "#" id) :value value})
(defn grove--browser-replace [target children]
  {:op "replace" :target target :children (into [] children)})
(defn grove--browser-li [text] (grove--browser-node "li" text {} []))
(defn grove--browser-details [value] (json-stringify value))
(defn grove--browser-button [label action value]
  (grove--browser-node "button" label {:type "button" :data-event action :data-value value} []))
(defn grove--browser-cell [text label]
  (grove--browser-node "td" text {:data-label label} []))
(defn grove--browser-table [id labels rows cells empty-text]
  (grove--browser-replace (str "#" id " tbody")
    (if (empty? rows)
      [(grove--browser-node "tr" "" {} [(grove--browser-node "td" empty-text {:colspan (str (count labels))} [])])]
      (map (fn [row]
        (let [values (cells row)]
          (grove--browser-node "tr" "" {}
            (map (fn [i]
              (let [v (nth values i)]
                (if (map? v) v (grove--browser-cell (str v) (nth labels i))))
              (range (count labels)))))) rows))))
(defn grove--browser-svg [target width height markup]
  {:op "svg" :target target :width width :height height :markup markup})
(defn grove--browser-http [event purpose output method path query body context]
  (let [token (grove--browser-field event "token")]
    {:op "http" :authority "same-origin" :method method :path path :query query
     :headers (if (= token "") {} {:Authorization (str "Bearer " token)})
     :body body :context (assoc context :purpose purpose :output output)}))
(defn grove--browser-result [state commands] {:state state :commands (into [] commands)})

(defn grove--browser-refresh [state event]
  (grove--browser-result state
    (into
      (map (fn [spec]
        (grove--browser-http event (nth spec 0) "runtime-status" "GET" (nth spec 1) {} nil {}))
        [["predictions" ["api" "predictions"]] ["runs" ["api" "runs"]]
         ["lineage" ["api" "lineage"]] ["modules" ["api" "modules"]]
         ["approvals" ["api" "logic" "candidates"]]
         ["publication" ["api" "learning" "publication"]]])
      (map (fn [id] (grove--browser-http event "signal-state" "signal-out" "GET"
        ["api" "signals" id] {} nil {:id id})) (grove--browser-get state "filed" [])))))

(defn grove--browser-pixels [orientation]
  ;; The exact observation pixel synthesis sent to /api/observations.
  (into [] (map (fn [index]
    (let [row (quot index 16) col (mod index 16)]
      (if (and (>= row 3) (< row 6) (if (= orientation "horizontal") (< col 10) true)) 220 12)))
    (range 256))))
(defn grove--browser-image [event]
  {:op "canvas" :target "#observation-image" :width 16 :height 16
   :pixels (grove--browser-pixels (grove--browser-field event "obs-bar"))})

(defn grove--browser-poll [state event]
  (let* [run (grove--browser-field event "event-run")
         same (= run (grove--browser-get state "event-run" ""))
         cursor (if same (grove--browser-get state "cursor" nil) nil)
         next-state (if same state (assoc state :event-run run :cursor nil :events [] :loss [] :polling false))]
    (if (= run "") (error "Name a run to read its durable trace")
      (if (grove--browser-get next-state "polling" false)
        (grove--browser-result next-state [])
        (grove--browser-result (assoc next-state :polling true)
          [(grove--browser-http event "events" "events-out" "GET" ["api" "events"]
             (if (nil? cursor) {:run_id run} {:run_id run :after_sequence cursor}) nil
             {:run run :cursor cursor})])))))

(defn grove--browser-loss-curve [loss width height]
  ;; Real loss samples render as an SVG polyline; empty trace → labelled placeholder.
  (if (< (count loss) 2)
    (grove--browser-svg "#loss-curve" width height
      (str "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 " width " " height
        "' role='img' aria-label='Loss curve not yet recorded'>"
        "<rect width='100%' height='100%' fill='#fff'/>"
        "<text x='8' y='" (/ height 2)
        "' font-family='ui-sans-serif,system-ui,sans-serif' font-size='12' fill='#5b6470'>"
        "loss samples appear after the first recorded step</text></svg>"))
    (let* [max-loss (reduce (fn [a x] (if (> x a) x a)) 0 loss)
           min-loss (reduce (fn [a x] (if (< x a) x a)) max-loss loss)
           span (if (= max-loss min-loss) 1 (- max-loss min-loss))
           step-x (/ width (- (count loss) 1))
           step-y (/ (- height 16) span)
           points (map (fn [i]
             (let [v (nth loss i)]
               (str (+ 8 (* i step-x)) "," (- height 8 (* (- v min-loss) step-y)))))
             (range (count loss)))
           polyline (reduce (fn [a p] (str a " " p)) (str (first points)) (rest points))]
      (grove--browser-svg "#loss-curve" width height
        (str "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 " width " " height
          "' role='img' aria-label='Recorded training loss curve'>"
          "<rect width='100%' height='100%' fill='#fff' stroke='#d5dae1'/>"
          "<polyline fill='none' stroke='#1f4f8f' stroke-width='1.5' points='" polyline "'/>"
          "<text x='8' y='12' font-family='ui-sans-serif,system-ui,sans-serif' font-size='10' fill='#5b6470'>"
          "loss " min-loss "→" max-loss " across " (count loss) " steps</text></svg>")))))

(defn grove--browser-submit [state event]
  (let* [id (grove--browser-get event "id" "")
         nonce (grove--browser-get event "nonce" "")
         f (fn [key] (grove--browser-field event key))
         post (fn [purpose output path body context]
           (grove--browser-result state [(grove--browser-http event purpose output "POST" path {} body context)]))]
    (cond
      (= id "observe-form")
      (let* [mask (map (fn [s]
                    (cond (= s "true") true (= s "false") false
                      :else (error "Modality mask must contain true or false")))
                    (map str-trim (str-split "," (f "obs-mask"))))
             readings (map grove--browser-number (map str-trim (str-split "," (f "obs-readings"))))]
        (if (not (and (= (count mask) 2) (= (count readings) 2)))
          (error "Observation needs two modality flags and two sensor readings")
          (grove--browser-result state
            [(grove--browser-image event)
             (grove--browser-http event "observe" "observe-out" "POST" ["api" "observations"] {}
              {:schema 1 :operation_id (str "obs-" nonce) :id (f "obs-id") :owner "<token-derived>"
               :task_id (f "obs-task") :source "web-ui" :occurred_at_ms 0
               :blocks [{:media_type "image" :artifact "0000000000000000000000000000000000000000000000000000000000000000"}]
               :modality_mask (into [] mask) :training_permitted true} {:id (f "obs-id")})])))
      (= id "predict-form")
      (post "predict" "predict-out" ["api" "predictions"]
        {:schema 1 :id (str "p-" nonce) :observation_id (f "pred-observation")
         :snapshot_digest (f "pred-snapshot") :snapshot_schema 1 :output {} :abstained false :created_at_ms 0} {})
      (= id "signal-form")
      (post "signal" "signal-out" ["api" "signals"]
        {:schema 1 :operation_id (str "sig-" nonce) :id (f "sig-id")
         :idempotency_key (f "sig-id") :kind (f "sig-kind")
         :observation_id nil :prediction_id (grove--browser-nullable (f "sig-pred"))
         :target_field (grove--browser-nullable (f "sig-field")) :content (f "sig-content")
         :usage_permitted (= (grove--browser-get (grove--browser-get event "fields" {}) "sig-permission" false) true)
         :occurred_at_ms 0 :received_at_ms 0 :revises nil} {:id (f "sig-id")})
      (= id "select-form")
      (post "select" "select-out" ["api" "learning" "select"]
        {:protocol_id (f "sel-protocol") :snapshot_digests (grove--browser-csv (f "sel-snapshots"))}
        {:protocol (f "sel-protocol")})
      (= id "evaluation-form")
      (grove--browser-result state [(grove--browser-http event "evaluations" "evaluation-out" "GET"
        ["api" "evaluations" (f "eval-snapshot")] {} nil {})])
      (= id "queue-form")
      (let* [spec (json-parse (f "queue-spec"))
             source (grove--browser-get spec "source" nil)
             training (grove--browser-get spec "training" nil)]
        (if (not (map? spec)) (error "Work must be a JSON object")
          (if (= (nil? source) (nil? training)) (error "Queue exactly one source or training specification")
            (post "queue" "queue-out" ["api" "learning" "queue"]
              (assoc spec :operation_id (str "queue-" nonce)) {}))))
      (= id "control-form")
      (let* [action (f "control-action")
             budget (grove--browser-natural (f "control-budget"))
             op (str action "-" nonce)]
        (cond
          (= action "cancel") (post "control" "control-out" ["api" "learning" "runs" (f "control-run") "cancel"] {:operation_id op :reason nil} {})
          (= action "pause") (post "control" "control-out" ["api" "learning" "pause"] {:operation_id op :run_id (f "control-run") :state_artifact nil} {})
          (= action "resume") (post "control" "control-out" ["api" "learning" "resume"]
            {:operation_id op :run_id (f "control-id") :checkpoint_id (f "control-checkpoint") :steps_budget budget} {})
          (= action "fork") (post "control" "control-out" ["api" "learning" "fork"]
            {:operation_id op :checkpoint_id (f "control-checkpoint") :branch_id (f "control-branch") :quota budget} {})
          :else (error "Unknown checkpoint action")))
      (= id "propose-form")
      (post "propose" "propose-out" ["api" "logic" "propose"]
        {:schema 1 :id (f "candidate-id") :parent_logic_ref (grove--browser-nullable (f "candidate-parent"))
         :module_path (f "candidate-path")
         :source_ref (grove--browser-get (grove--browser-get event "fields" {}) "candidate-source-ref" "")
         :declared_dependencies (grove--browser-csv (f "candidate-deps"))
         :diff_ref (grove--browser-get (grove--browser-get event "fields" {}) "candidate-diff-ref" "")
         :evidence_refs (grove--browser-csv (f "candidate-evidence")) :created_at_ms 0} {})
      (= id "evaluate-form")
      (post "evaluate" "evaluate-out" ["api" "logic" "evaluate"]
        {:candidate_id (f "review-id") :protocol_id (f "review-protocol") :seeds [] :gates []
         :timeout_counts_as_failure true :device "cpu"} {})
      (= id "publish-form")
      (let [body {:operation_id (str "approve-" nonce)
                  :snapshot (f "pub-snapshot") :protocol_id (f "pub-protocol")
                  :expected_version (if (= (f "pub-version") "") nil (grove--browser-natural (f "pub-version")))}]
        ;; The controller MUST require an explicit confirm; it must not auto-approve
        ;; after evaluation completes. Approval is gated on a confirmed human action.
        (if (= (get body :snapshot) "") (error "Snapshot digest required")
          (grove--browser-result state [{:op "confirm" :message
            (str "Publish snapshot " (get body :snapshot) " under protocol " (get body :protocol_id)
              " at expected version " (get body :expected_version) "? This replaces the model for every reader and records your authenticated publisher decision.")
            :context {:action "approve" :body body}}])))
      :else (error (str "Unknown form " id)))))

(defn grove--browser-render [purpose data context]
  (let [g (fn [m k] (grove--browser-get m k nil))]
    (cond
      (= purpose "predictions")
      [(grove--browser-table "predictions" ["Prediction" "Snapshot" "Output" "Abstained" "Correct"] data
        (fn [p] [(g p "id") (g p "snapshot_digest") (g p "output") (g p "abstained")
          (grove--browser-button "Correct" "correct" (g p "id"))]) "No predictions recorded.")]
      (= purpose "runs")
      [(grove--browser-table "runs" ["Run" "State" "Steps" "Resumed from"] data
        (fn [r] [(g r "id") (g r "state") (str (g r "steps_consumed") "/" (g r "steps_budget"))
                  (grove--browser-get r "resumed_from" "—")]) "No runs.")]
      (= purpose "lineage")
      (let [nodes (grove--browser-get data "nodes" [])
            edges (grove--browser-get data "edges" [])
            pub (g data "publication")]
        [(grove--browser-replace "#lineage"
          (map (fn [n]
            (grove--browser-node "li"
              (if (= (g n "kind") "checkpoint")
                (str "Checkpoint " (g n "id") " · run " (g n "run_id") " · " (g n "budget_spent_steps") " steps · "
                  (g n "resume_level")
                  (if (g n "recoverable") " · recoverable" " · NOT RECOVERABLE (state artifact gone)"))
                (str (g n "id") " · " (g n "snapshot_digest")
                  (if (g n "deployable") " · deployable" " · INVALIDATED, not deployable")
                  " · revoked signals " (grove--browser-details (grove--browser-get n "revoked_signals" []))))
              {}
              [(grove--browser-node "ul" "" {}
                (map (fn [edge] (grove--browser-li (str "via " (g edge "kind") " ← " (g edge "from"))))
                  (filter (fn [edge] (= (g edge "to") (g n "id"))) edges)))]))
            nodes))
         (grove--browser-text "publication" (if pub (str "Active publication v" (g pub "version") " → " (g pub "snapshot_digest")) "No active publication.") false)])
      (= purpose "modules")
      ;; /api/modules returns {snapshot, snapshot_schema, modules:[ModuleSpec...]}
      [(grove--browser-replace "#modules"
        (map (fn [m]
          (grove--browser-node "li" (str (g m "name")
            ": " (g m "input_space") " → " (g m "output_space")
            (if (g m "frozen") " · frozen" " · trainable")
            " · shared group " (g m "shared_group")
            " · requires " (g m "requires")
            " · dependencies " (grove--browser-details (g m "depends_on"))) {} []))
          (grove--browser-get data "modules" [])))
       (grove--browser-text "modules-snapshot" (str "Snapshot " (or (g data "snapshot") "—") " (schema v" (g data "snapshot_schema" 1) ")") false)])
      (= purpose "approvals")
      ;; /api/logic/candidates returns {candidates:[LogicCandidate...]}.
      [(grove--browser-replace "#approvals" (map (fn [a]
        (grove--browser-li (str (g a "id") " · " (g a "module_path") " · parent " (g a "parent_logic_ref")
          " · state " (g a "state") " · created " (g a "created_at_ms"))))
        (grove--browser-get data "candidates" [])))]
      (= purpose "publication")
      ;; /api/learning/publication returns {active, history:[{version,snapshot_digest,approved_by,approved_at_ms,protocol_id}]}
      [(grove--browser-replace "#publication-history"
        (map (fn [h]
          (grove--browser-li (str "v" (g h "version") " · " (g h "snapshot_digest")
            " · protocol " (g h "protocol_id") " · approved by " (g h "approved_by")
            " · at " (g h "approved_at_ms"))))
        (grove--browser-get data "history" [])))
       (grove--browser-text "publication" (let [a (g data "active")]
        (if a (str "Active publication v" (g a "version") " → " (g a "snapshot_digest"))
          "No active publication; select an explicit snapshot digest")) false)]
      (= purpose "select")
      ;; /api/learning/select returns {comparisons:[Comparison], non_dominated:[snapshot], protocol_id}
      [(grove--browser-table "comparison" ["Snapshot" "Repeats" "Mean metrics" "Gates" "Select"]
        (grove--browser-get data "comparisons" [])
        (fn [r] [(g r "snapshot_digest") (g r "repeats") (grove--browser-details (g r "mean"))
          (if (g r "meets_gates") "met" (str "FAILED " (grove--browser-details (g r "gate_failures"))))
          (grove--browser-button "Review this snapshot" "select-snapshot"
            (json-stringify {:snapshot (g r "snapshot_digest") :protocol (g context "protocol")}))])
        "No comparisons.")
       (grove--browser-text "select-out" (str "Protocol " (g data "protocol_id") " · non-dominated: "
        (grove--browser-details (g data "non_dominated"))) false)]
      (= purpose "evaluations")
      ;; /api/evaluations/:snapshot returns {snapshot, repeats, mean, meets_gates, gate_failures, repeats_history:[EvaluationRecord...]}
      (let [records (grove--browser-get data "repeats_history" [])]
        [(grove--browser-replace "#evaluations" (map (fn [e]
          (grove--browser-li (str "Protocol " (g e "protocol_id") " · repeat " (g e "repeat_index") " · device " (g e "device")
            " · metrics " (grove--browser-details (g e "metrics"))))) records))])
      (= purpose "signal-state")
      []
      :else [])))

(defn grove--browser-response [state event]
  (let* [context (grove--browser-get event "context" {})
         purpose (grove--browser-get context "purpose" "")
         output (grove--browser-get context "output" "runtime-status")
         status (grove--browser-get event "status" 0)
         raw (grove--browser-get event "body" "")
         data (try (if (= raw "") nil (json-parse raw)) (catch any {:detail raw :class "invalid-response"}))
         g (fn [m k] (grove--browser-get m k nil))]
    (if (or (< status 200) (>= status 300))
      (grove--browser-result (if (= purpose "events") (assoc state :polling false) state)
        [(grove--browser-text output (str "HTTP " status " · " (grove--browser-get data "class" "transport-error") " · "
          (grove--browser-get data "detail" (grove--browser-get event "error" raw))
          (if (g data "conflict_version") (str " · current version " (g data "conflict_version")) "")) true)])
      (cond
        (= purpose "events")
        ;; Drop pages whose context no longer matches the active run/cursor (stale response).
        (if (not (and (= (g context "run") (g state "event-run")) (= (g context "cursor") (g state "cursor"))))
          (grove--browser-result state [])
          (let* [old (grove--browser-get state "events" [])
                 incoming (grove--browser-get data "events" data)
                 events (into old incoming)
                 cursor (g data "cursor" (if (empty? events) nil (- (count events) 1)))
                 loss (into (grove--browser-get state "loss" [])
                            (filter (fn [x] (some? x)) (map (fn [e] (grove--browser-parse-loss (g e "detail"))) incoming)))
                 next-state (assoc state :polling false :events events :cursor cursor :loss loss)]
            (grove--browser-result next-state
              [(grove--browser-replace "#events" (map (fn [e] (grove--browser-li
                (str (g e "sequence") " · +" (g e "at_ms") "ms · " (g e "kind") " · " (g e "detail")))) events))
               (grove--browser-loss-curve loss 320 96)
               (grove--browser-text "events-out" (str "Read through sequence " cursor "; traces, loss and checkpoint events are recorded, not simulated.") false)])))
        (= purpose "signal-state")
        (let* [id (g context "id")
               signals (assoc (grove--browser-get state "signals" {}) id data)
               next-state (assoc state :signals signals)]
          (grove--browser-result next-state
            [(grove--browser-table "signals" ["Signal" "Status" "In force" "Learned into"] (keys signals)
              (fn [k] (let [s (get signals k)]
                [k (g s "status") (if (g s "in_force") "yes" "no")
                  (if (empty? (g s "learned_into")) "not yet" (grove--browser-details (g s "learned_into")))]))
              "No signals.")]))
        (= purpose "signal")
        (let* [id (g data "id" (g context "id"))
               filed (grove--browser-get state "filed" [])
               next-filed (if (> (count (filter (fn [x] (= x id)) filed)) 0) filed (vector-conj filed id))
               next-state (assoc state :filed next-filed)]
          (grove--browser-result next-state
            [(grove--browser-text output (str "Signal " id " " (g data "status") "; filed does not mean learned.") false)
             {:op "storage-set" :key "grove.signals" :value (json-stringify next-filed)}
             (grove--browser-http event "signal-state" "signal-out" "GET" ["api" "signals" id] {} nil {:id id})]))
        (= purpose "observe")
        (grove--browser-result state [(grove--browser-text output (str "Observation " (g data "id") " accepted") false)
          (grove--browser-value "pred-observation" (grove--browser-get data "id" (g context "id")))])
        (= purpose "active")
        (let [pub (g data "publication")]
          (grove--browser-result state (if pub [(grove--browser-value "pred-snapshot" (g pub "snapshot_digest"))
            (grove--browser-text output (str "Active model v" (g pub "version") " selected") false)]
            [(grove--browser-text output "No active publication; select an explicit snapshot digest" true)])))
        (= purpose "predict")
        (grove--browser-result state [(grove--browser-text output
          (str (if (g data "abstained") "Abstained" (str "Output " (grove--browser-details (g data "output"))))
            " · " (g data "id") " · snapshot " (g data "snapshot_digest")) false)
          (grove--browser-http event "predictions" "predict-out" "GET" ["api" "predictions"] {} nil {})])
        (= purpose "evaluate")
        (grove--browser-result state [(grove--browser-text output
          (str "Candidate " (g data "candidate_id") " · " (g data "status") " · publication unchanged") false)
          (grove--browser-text "review-evidence" (grove--browser-details data) false)])
        (= purpose "propose")
        (grove--browser-result state [(grove--browser-value "review-id" (g data "id"))
          (grove--browser-text output (grove--browser-details data) false)
          (grove--browser-text "review-evidence" (grove--browser-details data) false)])
        (= purpose "approve")
        (let [refreshed (grove--browser-refresh state event)]
          (grove--browser-result state (into [(grove--browser-text output
            (str "Approved · immutable publication v" (g data "version") " is active") false)] (get refreshed :commands))))
        (or (= purpose "queue") (= purpose "control"))
        (grove--browser-result state [(grove--browser-text output (grove--browser-details data) false)
          (grove--browser-http event "runs" output "GET" ["api" "runs"] {} nil {})])
        :else (grove--browser-result state (grove--browser-render purpose data context))))))

(defn grove--browser-dispatch [state event]
  (let* [kind (grove--browser-get event "kind" "")
         id (grove--browser-get event "id" "")]
    (cond
      (= kind "boot")
      (grove--browser-result {:filed [] :signals {} :event-run "" :events [] :loss [] :cursor nil :polling false}
        [(grove--browser-text "runtime-status" "Zio/WASM controller ready · explicit AST bootstrap" false)
         {:op "attribute" :target "#surface" :name "aria-busy" :value "false"}
         (grove--browser-value "obs-id" "obs-1") (grove--browser-value "obs-task" "geometry-sensor-xor@1.0.0")
         (grove--browser-value "obs-mask" "true,true") (grove--browser-value "obs-readings" "1.2,-0.8")
         (grove--browser-value "pred-observation" "obs-1") (grove--browser-value "sig-id" "corr-1")
         (grove--browser-value "sel-protocol" "accept-v1") (grove--browser-value "review-protocol" "accept-v1")
         (grove--browser-value "pub-protocol" "accept-v1") (grove--browser-value "control-budget" "100")
         (grove--browser-value "queue-spec" (json-stringify {:kind "zios" :source "(+ 1 2)" :steps_budget 100 :run_id "run-1" :task_id "geometry-sensor-xor@1.0.0"}))
         {:op "storage-get" :key "grove.signals" :context {:action "restore-signals"}}
         (grove--browser-image {:fields {"obs-bar" "horizontal"}})
         (grove--browser-loss-curve [] 320 96)])
      (= kind "storage")
      (let* [raw (grove--browser-get event "value" "[]")
             filed (try (json-parse (if (nil? raw) "[]" raw)) (catch any []))]
        (grove--browser-refresh (assoc state :filed (if (vector? filed) (into [] (filter string? filed)) [])) event))
      (= kind "submit") (grove--browser-submit state event)
      (= kind "response") (grove--browser-response state event)
      (= kind "confirm")
      ;; Approval: the controller MUST wait for an explicit, accepted confirmation.
      ;; A refused/cancelled confirm clears any pending approval. Tokens never stored.
      (if (and (grove--browser-get event "accepted" false)
               (= (grove--browser-get (grove--browser-get event "context" {}) "action" "") "approve"))
        (grove--browser-result state [(grove--browser-http event "approve" "publish-out" "POST" ["api" "approve"] {}
          (grove--browser-get (grove--browser-get event "context" {}) "body" {}) {})])
        (grove--browser-result state [(grove--browser-text "publish-out" "Approval cancelled; publication unchanged" false)]))
      (= kind "timer") (grove--browser-poll state event)
      (and (= kind "change") (= id "obs-bar")) (grove--browser-result state [(grove--browser-image event)])
      (or (= kind "click") (= kind "change"))
      (cond
        (or (= id "refresh") (= id "token")) (grove--browser-refresh state event)
        (= id "use-active") (grove--browser-result state [(grove--browser-http event "active" "predict-out" "GET" ["api" "lineage"] {} nil {})])
        (= id "correct") (grove--browser-result state [(grove--browser-value "sig-pred" (grove--browser-get event "value" "")) {:op "focus" :target "#sig-content"}])
        (= id "select-snapshot")
        (let [selected (try (json-parse (grove--browser-get event "value" "{}")) (catch any {}))]
          (grove--browser-result state [(grove--browser-value "pub-snapshot" (grove--browser-get selected "snapshot" ""))
            (grove--browser-value "pub-protocol" (grove--browser-get selected "protocol" "")) {:op "focus" :target "#pub-version"}]))
        (= id "poll-events") (grove--browser-poll state event)
        (= id "watch-events")
        (let [result (grove--browser-poll state event)]
          (grove--browser-result (get result :state) (vector-conj (get result :commands) {:op "timer" :key "trace" :milliseconds 2000})))
        (= id "stop-events") (grove--browser-result state [{:op "timer-cancel" :key "trace"}])
        :else (grove--browser-result state []))
      :else (grove--browser-result state []))))

(defn grove--browser-step [state event]
  (try (grove--browser-dispatch state event)
    (catch any
      (let [id (grove--browser-get event "id" "")
            outputs {"observe-form" "observe-out" "predict-form" "predict-out" "signal-form" "signal-out"
              "select-form" "select-out" "publish-form" "publish-out" "queue-form" "queue-out" "control-form" "control-out"
              "propose-form" "propose-out" "evaluate-form" "evaluate-out" "evaluation-form" "evaluation-out"}]
        (grove--browser-result state [(grove--browser-text (get outputs id "runtime-status") (str *error*) true)])))))
`;
export const BUNDLE_PATH = '/zio/apps/grove/browser-bundle.zio';
export const BUNDLE_SOURCE = `;; Bundle entry the Foundation WASM build evaluates at startup.
;;
;; AST startup is explicit: the Foundation build reads this file with the
;; AST evaluator. Compiled execution is acceptable once the Compiler agent
;; lands; nothing here implies compilation is live.
;;
;; The bundle returns a JSON map the bridge consumes:
;;   { initialState:    <the controller's persisted state>
;;     initialCommands: <the command list from the boot event> }
;; The bridge applies the initial commands immediately and uses
;; initialState as the seed for every subsequent event.

;; Load the controller via load; the Foundation build embedded
;; /zio/apps/grove/browser-controller.zio via add_source, so this
;; resolves. The browser IoHost's "no cwd" capability short-circuits
;; relative load resolution, so the path is absolute.
(load "/zio/apps/grove/browser-controller.zio")

(defn grove--browser-boot []
  ;; Dispatch the synthetic boot event through the public step entry so
  ;; the controller's own boot path produces the welcome command list.
  (grove--browser-step {} {:kind "boot" :fields {}}))

(defn grove--browser-export []
  ;; Single expression so eval_json returns it as the bundle value.
  (let* [boot (grove--browser-boot)]
    {:initialState (get boot :state)
     :initialCommands (get boot :commands)
     :version "grove.bundle/1"
     :startup "ast"}))
`;
