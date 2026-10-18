<script setup lang="ts">
/**
 * LauncherIcon — плитка с логотипом лаунчера.
 *
 * Логотипы квадратные 256px с прозрачностью, поэтому лежат на подложке
 * `white/5` с тонкой рамкой: иначе светлые значки (PolyMC, Modrinth) теряются
 * на тёмном фоне, а тёмные (Prism) — сливаются с ним.
 */
import { computed } from 'vue';
import { LAUNCHER_ICONS } from '../../assets/launchers';
import { LAUNCHER_NAMES, type LauncherKind } from '../types/installer';

const props = withDefaults(defineProps<{ kind: LauncherKind; size?: 'sm' | 'md' }>(), {
  size: 'md',
});

const src = computed(() => LAUNCHER_ICONS[props.kind]);
const box = computed(() => (props.size === 'sm' ? 'h-9 w-9 rounded-lg' : 'h-12 w-12 rounded-xl'));
const img = computed(() => (props.size === 'sm' ? 'h-5 w-5' : 'h-7 w-7'));
</script>

<template>
  <span
    class="flex shrink-0 items-center justify-center border border-white/10 bg-white/5"
    :class="box"
  >
    <img
      :src="src"
      :alt="LAUNCHER_NAMES[kind]"
      class="object-contain"
      :class="img"
      draggable="false"
    />
  </span>
</template>
