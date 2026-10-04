// Zio sources evaluated live in the browser. They are Zio code, not mock
// output: the reader has no string escapes, so mock LLM payloads are map
// literals. Validated by running against the shipped wasm engine.
export const scenarios: Record<string, string> = {
  toolAgent: `;; ── Tool-Calling Agent Loop（模拟 LLM 路由，本地离线规则）──
(def env {:weather {:city "Hangzhou" :temp-c 24 :cond "sunny"}
          :calc    {:expr "40 + 2" :value 42}})

(defn route [msg]
  (cond
    ((str-contains? msg "天气") :weather)
    ((str-contains? msg "计算") :calc)
    (else :none)))

(defn agent [msg]
  (let* [tool (route msg)]
    (cond
      ((= tool :weather)
       (let* [r (get env :weather)]
         (str "Tool[weather] → " (get r :city) " " (get r :temp-c) "°C " (get r :cond))))
      ((= tool :calc)
       (let* [r (get env :calc)]
         (str "Tool[calc] → " (get r :expr) " = " (get r :value))))
      (else (str "LLM → " msg " | 无可用工具，直接回答")))))

(list
  (agent "查一下 Hangzhou 天气")
  (agent "帮我计算 40 + 2")
  (agent "写一首诗"))`,

  cspAgents: `;; ── 多 Agent CSP 通道协作 ──
(def bus (chan 8))

(defn worker [name payload]
  (do
    (send! bus {:from name :msg payload})
    (str name " 已投递: " payload)))

(def a (worker "planner" "拆解任务: 抓取数据"))
(def b (worker "executor" "执行: 抓取数据"))

(def inbox (list (recv! bus) (recv! bus)))
(defn summarize [acc m]
  (str acc (get m :from) " → " (get m :msg) "；"))
(str "协调者收到 ⇒ " (reduce summarize "" inbox))`,

  llmPipeline: `;; ── LLM 结构化输出管道（模拟补全 → 提取 → JSON 序列化）──
(def mock-llm {:summary {:tldr "Zio 是通用 Lisp" :tokens 12}
               :extract {:entity "Zio" :score 95}})

(defn run-pipeline [kind text]
  (let* [c (get mock-llm kind)]
    (cond
      ((= kind :summary)
       (json-stringify {:kind "summary" :input text :tldr (get c :tldr) :tokens (get c :tokens)}))
      (else
       (json-stringify {:kind "extract" :input text :entity (get c :entity) :score (get c :score)})))))

(list
  (run-pipeline :summary "Zio 是一门通用 Lisp 语言")
  (run-pipeline :extract "Zio 与 Lisp 的关系"))`,
};
