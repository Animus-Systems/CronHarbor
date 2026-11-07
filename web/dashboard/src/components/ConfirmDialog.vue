<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from "vue";
const props = defineProps<{
  open: boolean;
  title: string;
  impact: string;
  confirmLabel?: string;
  reasonRequired?: boolean;
}>();
const emit = defineEmits<{ cancel: []; confirm: [reason: string] }>();
const dialog = ref<HTMLDialogElement>();
const reason = ref("");
let previousFocus: Element | null = null;
watch(
  () => props.open,
  async (open) => {
    await nextTick();
    if (open) {
      previousFocus = document.activeElement;
      reason.value = "";
      dialog.value?.showModal();
    } else {
      dialog.value?.close();
      if (previousFocus instanceof HTMLElement) previousFocus.focus();
    }
  },
);
onBeforeUnmount(() => {
  dialog.value?.close();
  if (previousFocus instanceof HTMLElement) previousFocus.focus();
});
</script>
<template>
  <dialog
    ref="dialog"
    aria-labelledby="confirm-title"
    @cancel.prevent="emit('cancel')"
  >
    <form @submit.prevent="emit('confirm', reason)">
      <p class="eyebrow">REVIEW ACTION</p>
      <h2 id="confirm-title">{{ title }}</h2>
      <p class="muted">{{ impact }}</p>
      <label v-if="reasonRequired" class="field"
        >Reason and evidence<textarea
          v-model="reason"
          required
          minlength="5"
          maxlength="4096"
          rows="4"
          placeholder="What did you verify outside CronHarbor?"
        />
      </label>
      <div class="dialog-actions">
        <button type="button" autofocus @click="emit('cancel')">
          Keep current state</button
        ><button
          class="danger-button"
          type="submit"
          :disabled="reasonRequired && reason.trim().length < 5"
        >
          {{ confirmLabel ?? "Confirm action" }}
        </button>
      </div>
    </form>
  </dialog>
</template>
