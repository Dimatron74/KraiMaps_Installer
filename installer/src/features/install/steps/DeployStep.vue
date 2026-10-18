<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import { LOADER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';
import ProgressBar from '../../../shared/ui/ProgressBar.vue';
import { formatBytes } from '../../../shared/utils/format';

const store = useInstallStore();

/** Порядок стадий деплоя: мир → моды → паки → слои. Провижининг загрузчика
 * (`installer-*`) идёт до деплоя и в счётчик не входит. */
const activeStages = computed(() => {
  const list = ['world'];
  if (store.pack?.is_modded) list.push('mods');
  if (store.selectedResourcePacks().length > 0) list.push('resourcepacks');
  list.push('layers');
  return list;
});

const isDeployStage = computed(() => activeStages.value.includes(store.progress?.stage ?? ''));

/** Идёт ли установка загрузчика — единственный этап, который можно отменить. */
const isInstallerStage = computed(
  () => store.progress?.stage === 'installer-run' || store.progress?.stage === 'installer-download',
);

/**
 * «Fabric 0.16.9» — загрузчик с версией, если она уже разрешена. Без неё
 * подпись оставляем обобщённой: выдуманная версия вводит в заблуждение
 * сильнее, чем её отсутствие.
 */
const loaderLabel = computed(() => {
  const pack = store.pack;
  if (!pack?.is_modded) return null;
  const name = LOADER_NAMES[pack.loader];
  return store.loaderVersion ? `${name} ${store.loaderVersion}` : name;
});

const stageName = computed(() => {
  const pack = store.pack;
  const loader = loaderLabel.value;
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
      return loader ? `Качаем установщик ${loader}…` : 'Качаем установщик загрузчика…';
    case 'installer-run':
      return loader && pack?.world_version
        ? `Устанавливаем ${loader} для Minecraft ${pack.world_version}…`
        : 'Устанавливаем загрузчик…';
    case 'version-finalize':
      return 'Завершаем подготовку версии…';
    default:
      return store.statusLine || 'Работаем…';
  }
});

/**
 * Пояснение под заголовком на время установки загрузчика. Без него минуты
 * ожидания выглядят как зависание, а по строкам лога пользователь решает, что
 * скачиваем именно мы — на самом деле работает официальный установщик
 * загрузчика. Называем серверы Mojang и разработчиков загрузчика, а не Forge:
 * для Fabric и Quilt это было бы неверно.
 */
const loaderHint = computed(() => {
  if (!isInstallerStage.value) return null;
  const loader = loaderLabel.value;
  return `Установку выполняет официальный установщик${loader ? ` ${loader}` : ' загрузчика'}: он скачивает компоненты с серверов Mojang и разработчиков загрузчика, это безопасно и занимает несколько минут.`;
});

const logOpen = ref(false);
const logBox = ref<HTMLElement | null>(null);

/** Держим хвост лога в виду — иначе «подробнее» показывает начало установки. */
watch(
  () => store.provisionLog.length,
  async () => {
    if (!logOpen.value) return;
    await nextTick();
    logBox.value?.scrollTo({ top: logBox.value.scrollHeight });
  },
);

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
  if (p.overall_total)
    return `${formatBytes(p.overall_done ?? 0)} из ${formatBytes(p.overall_total)}`;
  if (p.total_bytes > 0) {
    return `${formatBytes(p.done_bytes)} из ${formatBytes(p.total_bytes)}`;
  }
  return null;
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
    <p v-if="loaderHint" class="mx-auto mt-2 max-w-md text-center text-xs text-secondary">
      {{ loaderHint }}
    </p>
    <div class="mx-auto mt-6 max-w-md">
      <ProgressBar
        :percent="percent"
        :label="store.target ? `Цель: ${store.target.name}` : ''"
        :current-file="store.progress?.current_file"
      >
        <template v-if="isInstallerStage" #after>
          <button
            type="button"
            class="mt-px block cursor-pointer text-left font-mono text-[11px] text-tertiary underline underline-offset-2"
            @click="logOpen = !logOpen"
          >
            {{ logOpen ? 'скрыть подробнее' : 'подробнее' }}
          </button>
        </template>
      </ProgressBar>
    </div>

    <pre
      v-if="isInstallerStage && logOpen"
      ref="logBox"
      class="mx-auto mt-2 max-w-md max-h-48 overflow-auto rounded-lg bg-black/30 p-3 text-left font-mono text-[11px] leading-relaxed text-secondary scroll-mb-2"
      >{{ store.provisionLog.join('\n') }}</pre>

    <div v-if="isInstallerStage" class="mt-6 flex justify-center">
      <Btn variant="danger" :loading="store.cancelling" @click="store.cancelProvisioning()">
        Отмена установки
      </Btn>
    </div>

    <div v-if="isInstallerStage" class="mt-3 text-center">
      <Alert v-if="store.cancelling" tone="info" class="text-left">
        Останавливаем установщик и убираем скачанное — это займёт секунды.
      </Alert>
    </div>

    <p class="mt-4 text-center text-xs text-tertiary">Не закрывайте окно до конца установки.</p>
  </div>
</template>
