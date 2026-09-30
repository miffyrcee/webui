/**
 * 变体层：把语义化的 intent / size 收敛成 daisyUI 类名字符串。
 *
 * 之所以单独抽一层：模板里同一个按钮在十几处出现，但语义只有
 * "主要 / 危险 / 警告 / 中性" 四种；直接手写 `btn btn-error btn-sm`
 * 一旦要统一改尺寸，就得全文件搜替换。
 *
 * ⚠️ 下面的映射必须是**完整字面量**，不能写成 `btn-${suffix}` 这类拼接：
 * Tailwind 是按源文件文本扫描候选类名的，拼出来的类名它看不见，
 * 于是这些变体类的 CSS 根本不会生成（表现为徽章/按钮整片裸样式）。
 */
const BTN_INTENT = {
  primary: 'btn-primary',
  secondary: 'btn-secondary',
  accent: 'btn-accent',
  info: 'btn-info',
  success: 'btn-success',
  warning: 'btn-warning',
  error: 'btn-error',
  neutral: 'btn-neutral',
  ghost: 'btn-ghost',
};

const BADGE_INTENT = {
  primary: 'badge-primary',
  secondary: 'badge-secondary',
  accent: 'badge-accent',
  info: 'badge-info',
  success: 'badge-success',
  warning: 'badge-warning',
  error: 'badge-error',
  neutral: 'badge-neutral',
  ghost: 'badge-ghost',
};

const BTN_SIZE = {
  xs: 'btn-xs',
  sm: 'btn-sm',
  md: 'btn-md',
  lg: 'btn-lg',
};

const INPUT_SIZE = {
  xs: 'input-xs',
  sm: 'input-sm',
  md: '',
  lg: 'input-lg',
};

/**
 * @param {{intent?: keyof typeof BTN_INTENT, size?: keyof typeof BTN_SIZE, block?: boolean, outline?: boolean}} [options]
 */
export function btnVariants({ intent = 'primary', size = 'sm', block = false, outline = false } = {}) {
  return [
    'btn',
    outline ? 'btn-outline' : '',
    BTN_INTENT[intent] ?? BTN_INTENT.primary,
    BTN_SIZE[size] ?? BTN_SIZE.sm,
    block ? 'btn-block' : '',
  ]
    .filter(Boolean)
    .join(' ');
}

/**
 * 面板容器。显式写出底色/描边/阴影，不依赖 daisyUI `.card` 的默认观感，
 * 以保证与模板里手写的 `bg-base-100 border border-base-300 shadow-sm` 一致。
 */
export function cardVariants() {
  return 'card bg-base-100 border border-base-300 shadow-sm';
}

/** @param {{intent?: keyof typeof BADGE_INTENT}} [options] */
export function badgeVariants({ intent = 'primary' } = {}) {
  return `badge ${BADGE_INTENT[intent] ?? BADGE_INTENT.primary}`;
}

/** @param {{size?: keyof typeof INPUT_SIZE}} [options] */
export function inputVariants({ size = 'md' } = {}) {
  return ['input', 'input-bordered', 'w-full', INPUT_SIZE[size] ?? ''].filter(Boolean).join(' ');
}
