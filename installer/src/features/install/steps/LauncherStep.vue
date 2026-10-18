<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore, VERSION_LAUNCHERS } from '../../../app/stores/install';
import { LAUNCHER_NAMES, type LauncherInfo } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';
import LauncherIcon from '../../../shared/ui/LauncherIcon.vue';

const store = useInstallStore();

const cards = computed<LauncherInfo[]>(() => store.detection?.launchers ?? []);

/** «Версий: 3» / «Инстансов: 3», а для пустого лаунчера — что будет дальше. */
function summary(l: LauncherInfo): string {
  if (l.targets.length === 0) {
    return VERSION_LAUNCHERS.includes(l.kind)
      ? 'Версий пока нет — установим нужную'
      : 'Инстансов пока нет — создадим новый';
  }
  return VERSION_LAUNCHERS.includes(l.kind)
    ? `Версий: ${l.targets.length}`
    : `Инстансов: ${l.targets.length}`;
}
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Куда ставим?</h2>

    <Alert v-if="store.cancelledNotice" tone="info" class="mt-3">
      {{ store.cancelledNotice }}
    </Alert>
    <p class="mt-1 text-sm text-secondary">
      Выберите лаунчер. Показаны только те, что найдены на этом компьютере.
    </p>

    <div class="mt-4 space-y-2">
      <button
        v-for="l in cards"
        :key="l.kind + l.data_dir"
        type="button"
        class="glass-card group flex w-full items-center gap-3.5 p-3.5 text-left transition-colors hover:border-white/20 hover:bg-white/[0.04] cursor-pointer"
        @click="store.selectLauncher(l)"
      >
        <LauncherIcon :kind="l.kind" />

        <span class="min-w-0 flex-1">
          <span class="flex items-center gap-2">
            <span class="truncate text-sm font-semibold text-primary">
              {{ LAUNCHER_NAMES[l.kind] }}
            </span>
            <span
              v-if="l.portable"
              class="shrink-0 rounded-full bg-primary-500/15 px-2 py-0.5 text-[11px] font-bold text-primary-400"
            >
              Портативный
            </span>
            <span
              v-if="!l.app_found"
              class="shrink-0 rounded-full bg-warning-500/15 px-2 py-0.5 text-[11px] font-bold text-warning-400"
            >
              Только папка
            </span>
          </span>
          <span
            class="mt-0.5 block truncate font-mono text-[11px] text-tertiary"
            :title="l.data_dir"
          >
            {{ l.data_dir }}
          </span>
          <span class="mt-1 block text-xs text-secondary">{{ summary(l) }}</span>
          <span v-if="!l.app_found" class="mt-1 block text-xs text-tertiary">
            Приложение не найдено — карту поставить можно, но запустить лаунчер отсюда не выйдет.
          </span>
        </span>

        <svg
          class="h-4 w-4 shrink-0 text-tertiary transition-colors group-hover:text-primary-400"
          viewBox="0 0 16 16"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M6 3.5 10.5 8 6 12.5"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    </div>

    <Alert tone="info" class="mt-4">
      <p class="text-xs">
        Нет нужного лаунчера? Запустите его и нажмите «Повторить поиск» — запущенный лаунчер
        находится всегда.
      </p>
    </Alert>

    <div class="mt-4 flex gap-2">
      <Btn variant="secondary" :disabled="store.busy" @click="store.step = 'confirm'">Назад</Btn>
      <Btn variant="secondary" :loading="store.busy" @click="store.rescanLaunchers()">
        {{ store.busy ? store.statusLine || 'Ищем…' : 'Повторить поиск' }}
      </Btn>
    </div>
  </div>
</template>
