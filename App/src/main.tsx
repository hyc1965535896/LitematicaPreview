import { Component, type ErrorInfo, type ReactNode } from "react"
import { createRoot } from "react-dom/client"
import App from "./App"
import "./styles.css"

class AppErrorBoundary extends Component<
  { children: ReactNode },
  { error: string | null; recovered: string | null }
> {
  state: { error: string | null; recovered: string | null } = { error: null, recovered: null }

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) }
  }

  componentDidCatch(error: Error, information: ErrorInfo) {
    console.error("应用界面渲染失败。", error, information.componentStack)
    if (this.state.recovered === null) {
      this.setState({ error: null, recovered: error.message })
    }
  }

  render() {
    if (this.state.error !== null) {
      return (
        <main
          className="mx-auto max-h-full max-w-2xl overflow-auto p-6 text-[CanvasText] bg-[Canvas] leading-relaxed"
          role="alert"
        >
          <h1 className="text-2xl font-bold leading-snug mb-4">
            Litematica Preview 无法显示其界面。
          </h1>
          <p className="mb-4">你的投影文件没有被修改。</p>
          <pre className="my-4 p-4 border border-[GrayText] rounded-md whitespace-pre-wrap break-words">
            {this.state.error}
          </pre>
          <button onClick={() => this.setState({ recovered: this.state.error, error: null })}>
            返回主页
          </button>
        </main>
      )
    }
    if (this.state.recovered !== null) return <App initialError={this.state.recovered} />
    return this.props.children
  }
}

const root = document.getElementById("root")
if (!root) throw new Error("缺少应用根元素。")
createRoot(root).render(
  <AppErrorBoundary>
    <App />
  </AppErrorBoundary>,
)
