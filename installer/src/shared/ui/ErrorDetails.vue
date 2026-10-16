<script setup lang="ts">
import { ref } from 'vue';

defineProps<{
  code: string;
  message: string;
  details: string;
  hint: string;
}>();

const expanded = ref(false);
</script>

<template>
  <div class="rounded-xl border border-error-500/30 bg-error-500/10 p-4">
    <p class="text-sm font-semibold text-primary">{{ message }}</p>
    <p class="mt-1 text-xs text-secondary">{{ hint }}</p>
    <button
      type="button"
      class="mt-2 text-xs font-medium text-primary-400 hover:text-primary-300 transition-colors cursor-pointer"
      @click="expanded = !expanded"
    >
      {{ expanded ? 'Скрыть детали' : 'Подробнее' }}
    </button>
    <div v-if="expanded" class="mt-2 rounded-lg bg-black/30 p-3 font-mono text-[11px] text-tertiary break-anywhere">
      <p>Код: {{ code }}</p>
      <p class="mt-1">{{ details }}</p>
    </div>
  </div>
</template>
