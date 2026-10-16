<script setup lang="ts">
/**
 * BrandMark — знак «КК» из фирменного стиля KraiMaps.
 * Порт `BrandLogo variant="mark"` с сайта (kraimaps-site/frontend/components/ui/BrandLogo.vue):
 * векторная типографика вместо PNG — чётко на любом DPI, выравнивание по базовой
 * линии шрифта, ноль проблем с прозрачными полями картинки.
 */
withDefaults(defineProps<{ size?: 'sm' | 'md' | 'lg' }>(), { size: 'md' });
</script>

<template>
  <span class="brand-mark" :class="`brand-mark--${size}`" role="img" aria-label="KraiMaps">
    <span class="brand-mark__k">К</span><span class="brand-mark__rest">К</span>
  </span>
</template>

<style scoped>
.brand-mark {
  display: inline-flex;
  align-items: baseline;
  font-family: 'Inter', system-ui, -apple-system, 'Segoe UI', sans-serif;
  font-weight: 800;
  letter-spacing: -0.025em;
  line-height: 1;
  white-space: nowrap;
  user-select: none;
}

.brand-mark--sm { font-size: 18px; }
.brand-mark--md { font-size: 22px; }
.brand-mark--lg { font-size: 28px; }

.brand-mark__k {
  position: relative;
  display: inline-block;
  background: linear-gradient(135deg, #7dd3fc 0%, #0ea5e9 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.brand-mark__k::after {
  content: '';
  position: absolute;
  bottom: 6%;
  right: -8%;
  width: 16%;
  height: 16%;
  background: #7dd3fc;
  border-radius: 2px;
  box-shadow: 0 0 12px rgba(56, 189, 248, 0.4);
}

.brand-mark__rest {
  color: #ffffff;
}
</style>
