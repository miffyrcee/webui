/**
 * 变体层：基于 class-variance-authority，把语义化的 intent / size 收敛成 daisyUI 类名字符串。
 *
 * 之所以单独抽一层：模板里同一个按钮在十几处出现，但语义只有
 * "主要 / 危险 / 警告 / 中性" 四种；直接手写 `btn btn-error btn-sm`
 * 一旦要统一改尺寸，就得全文件搜替换。
 *
 * ⚠️ 下面的映射必须是**完整字面量**，不能写成 `btn-${suffix}` 这类拼接：
 * Tailwind 是按源文件文本扫描候选类名的，拼出来的类名它看不见，
 * 于是这些变体类的 CSS 根本不会生成（表现为徽章/按钮整片裸样式）。
 * cva 只是把这些字面量收进 variants 对象，类名字符串本身仍然原样留在本文件里，
 * 因此 `style.css` 的 `@source "../"` 依旧能扫到它们。
 */
import { cva } from 'class-variance-authority';

/** 按钮变体 */
export const btnVariants = cva(
  'btn transition-all duration-150 active:scale-95 cursor-pointer',
  {
    variants: {
      intent: {
        primary: 'btn-primary',
        secondary: 'btn-secondary',
        accent: 'btn-accent',
        info: 'btn-info',
        success: 'btn-success',
        warning: 'btn-warning',
        error: 'btn-error',
        neutral: 'btn-neutral',
        ghost: 'btn-ghost',
      },
      size: {
        xs: 'btn-xs',
        sm: 'btn-sm',
        md: 'btn-md',
        lg: 'btn-lg',
      },
      outline: {
        true: 'btn-outline',
      },
      block: {
        true: 'btn-block',
      },
    },
    defaultVariants: {
      intent: 'primary',
      size: 'sm',
      outline: false,
      block: false,
    },
  }
);

/**
 * 面板容器。显式写出底色/描边/圆角/阴影，不依赖 daisyUI `.card` 的默认观感，
 * 与模板里手写的 `bg-base-100 border border-base-300 rounded-2xl shadow-sm` 保持一致。
 */
export const cardVariants = cva(
  'card bg-base-100 border border-base-300 shadow-sm rounded-2xl overflow-hidden'
);

/** 徽章变体 */
export const badgeVariants = cva('badge font-semibold text-xs', {
  variants: {
    intent: {
      primary: 'badge-primary',
      secondary: 'badge-secondary',
      accent: 'badge-accent',
      info: 'badge-info',
      success: 'badge-success',
      warning: 'badge-warning',
      error: 'badge-error',
      neutral: 'badge-neutral',
      ghost: 'badge-ghost',
    },
    size: {
      xs: 'badge-xs',
      sm: 'badge-sm',
      md: 'badge-md',
      lg: 'badge-lg',
    },
  },
  defaultVariants: {
    intent: 'primary',
    size: 'sm',
  },
});

/** 输入框变体 */
export const inputVariants = cva(
  'input input-bordered w-full text-xs sm:text-sm focus:outline-primary',
  {
    variants: {
      size: {
        xs: 'input-xs',
        sm: 'input-sm',
        md: 'input-md',
      },
    },
    defaultVariants: {
      size: 'sm',
    },
  }
);
