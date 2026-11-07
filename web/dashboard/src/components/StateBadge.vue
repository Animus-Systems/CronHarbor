<script setup lang="ts">
import { tone } from "../format";
defineProps<{ state: string }>();
</script>
<template>
  <span class="badge" :class="tone(state)"
    ><svg aria-hidden="true" viewBox="0 0 16 16">
      <circle
        cx="8"
        cy="8"
        r="5"
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
      />
      <path
        v-if="tone(state) === 'success'"
        d="m5 8 2 2 4-4"
        fill="none"
        stroke="currentColor"
      />
      <path
        v-else-if="['warning', 'danger'].includes(tone(state))"
        d="M8 4.5v4M8 10v1"
        stroke="currentColor"
      />
      <path v-else d="M5.5 8h5" stroke="currentColor" /></svg
    >{{ state.replaceAll("_", " ") }}</span
  >
</template>
