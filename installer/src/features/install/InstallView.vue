<script setup lang="ts">
/** Экран установки: загрузка с сайта (прогресс) или обычный визард с файлом. */
import { computed } from 'vue';
import { useSiteInstallStore } from '../../app/stores/siteInstall';
import { useInstallStore } from '../../app/stores/install';
import InstallWizard from './InstallWizard.vue';
import Btn from '../../shared/ui/Btn.vue';
import ProgressBar from '../../shared/ui/ProgressBar.vue';
import ErrorDetails from '../../shared/ui/ErrorDetails.vue';
import { formatBytes } from '../../shared/utils/format';

const site = useSiteInstallStore();
const install = useInstallStore();

const showSiteProgress = computed(
  () => site.step === 'checking' || site.step === 'downloading' || site.step === 'handing',
);

const progressPct = computed(() => {
  const p = site.progress;
  if (!p || !p.total_bytes) return null;
  return Math.min(100, Math.round((p.done_bytes / p.total_bytes) * 100));
});

/** «Скачиваем файл… · 12,4 из 48,1 МБ» (без размера — только скачанное). */
const downloadLabel = computed(() => {
  if (site.step === 'checking') return 'Получаем данные…';
  const p = site.progress;
  if (!p) return 'Скачиваем файл…';
  const done = formatBytes(p.done_bytes);
  return p.total_bytes
    ? `Скачиваем файл… · ${done} из ${formatBytes(p.total_bytes)}`
    : `Скачиваем файл… · ${done}`;
});

function retry() {
  if (site.mapId !== null) void site.startFromSite(site.mapId, site.mode);
}
</script>

<template>
  <div class="flex flex-1 flex-col">
    <div v-if="showSiteProgress" class="flex flex-col items-center py-8 text-center">
      <div
        v-if="site.notice"
        class="mb-5 flex w-full max-w-md items-center gap-2.5 rounded-xl border border-success-500/30 bg-success-500/10 px-3.5 py-2.5 text-left"
      >
        <svg class="h-5 w-5 shrink-0 text-success-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M9 12.75L11.25 15 15 9.75M21 12a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
        <p class="text-sm font-semibold text-primary">{{ site.notice }}</p>
      </div>
      <h2 class="text-lg font-bold text-primary">
        {{ site.manifest?.title ?? 'Загружаем карту…' }}
      </h2>
      <p class="mt-1 text-sm text-secondary">
        <template v-if="site.step === 'checking'">Получаем данные карты…</template>
        <template v-else-if="site.step === 'downloading'">Скачиваем файл…</template>
        <template v-else>Готовим установку…</template>
      </p>
      <div class="mt-5 w-full max-w-md">
        <ProgressBar :percent="progressPct" :label="downloadLabel" />
      </div>
    </div>

    <div v-else-if="site.step === 'failed' && site.error" class="flex flex-col items-center py-8 text-center">
      <h2 class="text-lg font-bold text-primary">Не получилось</h2>
      <p class="mt-2 max-w-md text-sm text-secondary">{{ site.error.message }}</p>
      <p class="mt-1 max-w-md text-xs text-tertiary">{{ site.error.hint }}</p>
      <div class="mt-5 flex gap-2">
        <Btn @click="retry">Попробовать ещё раз</Btn>
      </div>
      <ErrorDetails
        class="mt-4 max-w-md"
        :code="site.error.code"
        :message="site.error.message"
        :details="site.error.details"
        :hint="site.error.hint"
      />
    </div>

    <InstallWizard v-else :key="install.zipPath || 'fresh'" />
  </div>
</template>
