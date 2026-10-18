<script setup lang="ts">
import { ref } from 'vue';

const props = defineProps<{
  code: string;
  message: string;
  details: string;
  hint: string;
  action?: { label: string; url: string };
}>();

const expanded = ref(false);
const opened = ref(false);

async function openAction() {
  const { openUrl } = await import('@tauri-apps/plugin-opener');
  await openUrl(props.action!.url);
  opened.value = true;
}
</script>

<template>
  <div class="rounded-xl border border-error-500/30 bg-error-500/10 p-4">
    <p class="text-sm font-semibold text-primary">{{ message }}</p>
    <p class="mt-1 text-xs text-secondary">{{ hint }}</p>
    <button
      v-if="action"
      type="button"
      class="mt-2 text-xs font-medium text-primary-400 hover:text-primary-300 transition-colors cursor-pointer"
      @click="openAction()"
    >
      {{ opened ? `Открыто: ${action.label}` : action.label }}
    </button>
    <button
      type="button"
      class="mt-2 text-xs font-medium text-primary-400 hover:text-primary-300 transition-colors cursor-pointer"
      :class="action ? 'ml-3' : ''"
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