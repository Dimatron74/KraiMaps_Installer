<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import ProgressBar from '../../../shared/ui/ProgressBar.vue';
import { formatBytes } from '../../../shared/utils/format';

const store = useInstallStore();

/** Порядок стадий деплоя: мир → моды → паки → слои. Провижининг TLauncher
 * (`installer-*`) идёт до деплоя и в счётчик не входит. */
const activeStages = computed(() => {
  const list = ['world'];
  if (store.pack?.is_modded) list.push('mods');
  if (store.selectedResourcePacks().length > 0) list.push('resourcepacks');
  list.push('layers');
  return list;
});

const isDeployStage = computed(() => activeStages.value.includes(store.progress?.stage ?? ''));

/**
 * Сквозной процент всего деплоя (байты, полоса идёт только вперёд).
 * Фолбэк — процент текущей стадии; `null` — без меры (indeterminate).
 */
const percent = computed(() => {
  const p = store.progress;
  if (!p) return null;
  if (p.overall_total) {
    return Math.min(100, ((p.overall_done ?? 0) / p.overall_total) * 100);
  }
  if (!p.total_bytes) return null;
  return Math.min(100, (p.done_bytes / p.total_bytes) * 100);
});

const stageCounter = computed(() => {
  const p = store.progress;
  if (!p || !isDeployStage.value) return null;
  return `Этап ${activeStages.value.indexOf(p.stage) + 1} из ${activeStages.value.length}`;
});

/** «12,4 из 48,1 МБ»: сквозные байты, иначе байты стадии. */
const mbLine = computed(() => {
  const p = store.progress;
  if (!p) return null;
  if (p.overall_total) return `${formatBytes(p.overall_done ?? 0)} из ${formatBytes(p.overall_total)}`;
  if (p.total_bytes > 0) {
    return `${formatBytes(p.done_bytes)} из ${formatBytes(p.total_bytes)}`;
  }
  return null;
});

const stageName = computed(() => {
  switch (store.progress?.stage) {
    case 'world':
      return 'Копируем мир…';
    case 'mods':
      return 'Ставим моды…';
    case 'layers':
      return 'Раскладываем настройки…';
    case 'resourcepacks':
      return 'Ставим ресурспаки…';
    case 'installer-download':
      return 'Качаем установщик загрузчика…';
    case 'installer-run':
      return 'Ставим версию игры — это займёт несколько минут…';
    case 'version-finalize':
      return 'Завершаем подготовку версии…';
    default:
      return store.statusLine || 'Работаем…';
  }
});
</script>

<template>
  <div class="py-8">
    <h2 class="text-center text-lg font-bold text-primary">{{ stageName }}</h2>
    <p v-if="stageCounter || mbLine" class="mt-1 text-center text-sm text-secondary">
      <template v-if="stageCounter">{{ stageCounter }}</template>
      <template v-if="stageCounter && mbLine"> · </template>
      <template v-if="mbLine">{{ mbLine }}</template>
    </p>
    <p v-if="store.statusLine && !store.progress" class="mt-1 text-center text-sm text-secondary">
      {{ store.statusLine }}
    </p>
    <div class="mx-auto mt-6 max-w-md">
      <ProgressBar
        :percent="percent"
        :label="store.target ? `Цель: ${store.target.name}` : ''"
        :current-file="store.progress?.current_file"
      />
    </div>
    <p class="mt-4 text-center text-xs text-tertiary">Не закрывайте окно до конца установки.</p>
  </div>
</template>
