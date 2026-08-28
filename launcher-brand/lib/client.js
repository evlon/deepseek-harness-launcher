// launcher-brand client half —— 覆盖 dsh web UI 的品牌名称。
//
// 机制：dsh web 的品牌由 `dsh-client-ui-brand-official` 注册到 slots 服务：
//   - sidebar.brand.mark     侧栏 logo 图标
//   - sidebar.brand.name     侧栏名称
//   - conversation.hero.brand.mark   会话头部 logo
// 本插件以**同名覆盖**注册 sidebar.brand.name，显示自定义名称（如「数字分身」），
// 避免与默认官方品牌（DeepSeek Harness 字标）冲突。
//
// 名称来源：本行 config.brandName（由 profile 的 cordis.patch.yml 配置），
// 缺失时回退到默认 'Harness'。

window.__ModuleLoader__.load({
  id: "launcher-brand",
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });

    /** Required service: the UI slot registry. */
    var inject = ["slots"];

    /** 品牌名称组件：从 slot props 读取配置，渲染纯文本名称。 */
    function BrandName() {
      return null; // 名称由注册时的 label 提供（见 apply 中 register 的 label）
    }

    /**
     * 读取本插件行的 config（brandName）。
     * dsh 的 client 插件通过 ctx.get 访问 host 注入的配置。
     */
    function brandNameOf(ctx) {
      try {
        var cfg = ctx.get("config");
        if (cfg && typeof cfg.brandName === "string" && cfg.brandName.length > 0) {
          return cfg.brandName;
        }
      } catch (e) {
        // 忽略：拿不到配置就用默认
      }
      return "Harness";
    }

    /** 渲染品牌名称文本（纯文本，无 logo）。 */
    function BrandNameText(props) {
      var name = (props && props.brandName) || "Harness";
      return React.createElement(
        "span",
        { className: "launcher-brand-name", style: { fontSize: "14px", fontWeight: 600 } },
        name
      );
    }

    function apply(ctx) {
      var name = brandNameOf(ctx);
      ctx.slots.inject("sidebar.brand.name", () => ctx.slots.register(
        { name: "sidebar.brand.name", id: "launcher-brand", order: 10 },
        function (props) {
          return BrandNameText(Object.assign({}, props, { brandName: name }));
        }
      ));
      // 覆盖会话头部的品牌名称（保留官方 mark，仅换名称）
      ctx.slots.inject("conversation.hero.brand.name", () => ctx.slots.register(
        { name: "conversation.hero.brand.name", id: "launcher-brand", order: 10 },
        function (props) {
          return BrandNameText(Object.assign({}, props, { brandName: name }));
        }
      ));
    }

    exports.apply = apply;
    exports.inject = inject;
    return module.exports;
  }
});
