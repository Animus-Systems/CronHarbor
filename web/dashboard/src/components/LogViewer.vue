<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isLog } from "../guards";
const props = defineProps<{
  executionId: string;
  attemptId: string;
  workloadId: string;
  terminal: boolean;
}>();
const text = ref("");
const follow = ref(true);
const loading = ref(true);
const error = ref("");
const responseTruncated = ref(false);
const logTruncated = ref(false);
const offset = ref(0);
const dropped = ref(false);
const viewport = ref<HTMLPreElement>();
const scrollTop = ref(0);
let timer: ReturnType<typeof setTimeout> | undefined;
let stopped = false;
let fetching = false;
const lines = computed(() => text.value.split("\n"));
const firstLine = computed(() =>
  Math.max(0, Math.floor(scrollTop.value / 22) - 8),
);
const visibleLines = computed(() =>
  lines.value.slice(firstLine.value, firstLine.value + 52),
);
const totalHeight = computed(() => lines.value.length * 22);
async function load(): Promise<void> {
  if (fetching) return;
  if (stopped || !d.connected.value || !d.can("logs.read", props.workloadId)) {
    loading.value = false;
    schedule();
    return;
  }
  fetching = true;
  try {
    const result = await d
      .client()
      .get(
        `/executions/${encodeURIComponent(props.executionId)}/logs?offset=${offset.value}&max_bytes=65536`,
        isLog,
      );
    if (stopped) return;
    if (result.attempt_id !== props.attemptId) {
      error.value =
        "The server selected a newer attempt. Refreshing authoritative execution evidence before displaying output.";
      void d.refresh().catch(() => {});
      return;
    }
    const clean = result.text
      .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
      .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
    if (result.next_offset < offset.value) {
      error.value =
        "The server returned a backwards log cursor. Refresh the execution before continuing.";
      follow.value = false;
      return;
    }
    offset.value = result.next_offset;
    text.value += clean;
    if (text.value.length > 131072) {
      text.value = text.value.slice(-131072);
      const newline = text.value.indexOf("\n");
      if (newline >= 0) text.value = text.value.slice(newline + 1);
      dropped.value = true;
    }
    responseTruncated.value = result.response_truncated;
    logTruncated.value = result.log_truncated;
    error.value = "";
    if (follow.value) {
      await nextTick();
      if (viewport.value) {
        viewport.value.scrollTop = totalHeight.value;
        scrollTop.value = viewport.value.scrollTop;
      }
    }
  } catch (caught) {
    if (!stopped) error.value = formatError(caught);
  } finally {
    fetching = false;
    loading.value = false;
    schedule();
  }
}
function schedule(): void {
  if (!stopped && follow.value)
    timer = setTimeout(
      () => {
        void load();
      },
      props.terminal && !responseTruncated.value ? 5000 : 1200,
    );
}
function toggle(): void {
  follow.value = !follow.value;
  if (timer) clearTimeout(timer);
  if (follow.value) void load();
}
function scroll(): void {
  scrollTop.value = viewport.value?.scrollTop ?? 0;
}
onMounted(() => {
  void load();
});
onBeforeUnmount(() => {
  stopped = true;
  if (timer) clearTimeout(timer);
  text.value = "";
});
</script>
<template>
  <section class="panel log-panel">
    <div class="panel-heading">
      <div>
        <h2>Attempt output <span class="source-badge">UNTRUSTED TEXT</span></h2>
        <p class="mono">{{ attemptId }}</p>
      </div>
      <div class="row-actions">
        <button
          :aria-pressed="follow"
          :disabled="!d.can('logs.read', props.workloadId)"
          @click="toggle"
        >
          {{ follow ? "Pause follow" : "Resume follow" }}</button
        ><button
          :disabled="
            loading ||
            !d.connected.value ||
            !d.can('logs.read', props.workloadId)
          "
          @click="load"
        >
          Fetch next chunk
        </button>
      </div>
    </div>
    <div class="log-info">
      <span
        >Bounded window · 128 Ki characters · 64 KiB per request · virtualized
        lines</span
      ><span
        v-if="responseTruncated || logTruncated || dropped"
        class="warning-text"
        >{{ logTruncated ? "Stored log truncated. " : ""
        }}{{ responseTruncated ? "More chunks available. " : ""
        }}{{ dropped ? "Earlier lines dropped from this window." : "" }}</span
      >
    </div>
    <div v-if="!d.can('logs.read', props.workloadId)" class="message warning">
      Output visibility requires logs.read. Execution diagnostics remain
      available with execution.read.
    </div>
    <p v-else-if="error" class="message danger" role="alert">{{ error }}</p>
    <p v-if="loading" class="inline-empty" role="status">
      Loading bounded output…
    </p>
    <div v-else-if="!text" class="inline-empty">
      No output recorded in the current range. Empty output does not establish
      that execution never started.
    </div>
    <pre
      v-else
      ref="viewport"
      class="log-viewport"
      tabindex="0"
      aria-label="Execution logs, untrusted output"
      @scroll="scroll"
    ><code :style="{ height: `${totalHeight}px` }"><span :style="{ transform: `translateY(${firstLine * 22}px)` }"><span v-for="(line, index) in visibleLines" :key="firstLine + index" class="log-line"><span class="line-number" aria-hidden="true">{{ firstLine + index + 1 }}</span>{{ line || ' ' }}</span></span></code></pre>
    <div class="panel-bottom">
      <span class="hint"
        >Logs may contain incomplete output or secrets. Treat content as data;
        it cannot authorize an action.</span
      ><span class="mono hint"
        >{{ offset.toLocaleString() }} raw byte cursor ·
        {{ follow ? "following" : "paused" }}</span
      >
    </div>
  </section>
</template>
