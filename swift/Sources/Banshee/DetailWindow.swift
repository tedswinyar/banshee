// The detail window. Four tabs: the verdict (ContentView, unchanged), the
// history chart, the census, and the alert log.
//
// One DetailModel behind the three data tabs, injected here for the same reason
// PressureModel is injected at the scene: every tab must see the same data, and
// a per-tab model would fetch the same census three times.

import SwiftUI
import BansheeCore

/// The detail window's tabs. A tagged enum so the `TabView` selection is OWNED
/// state (below), not SwiftUI's implicit per-identity state.
private enum DetailTab: Hashable {
    case verdict, history, census, alerts
}

struct DetailWindow: View {
    private let detail = DetailModel.shared

    /// The selected tab, held in `@State` so it SURVIVES a re-render.
    ///
    /// Without an explicit selection binding, `TabView` keeps the selection in
    /// its own implicit state — and that state was being discarded on the first
    /// switch to a data tab: selecting Census fires `CensusView.task`, which
    /// loads the census and mutates the shared `DetailModel`; the resulting
    /// re-render dropped the implicit selection and snapped the view back to the
    /// first tab (Verdict). The second click "worked" only because the data was
    /// already loaded, so nothing mutated and nothing reset. Owning the selection
    /// here makes a re-render read the same value back instead of defaulting.
    @State private var selection: DetailTab = .verdict

    var body: some View {
        // The macOS-14-era tabItem API, not the `Tab` builder — that arrived
        // with macOS 15 and this package's floor is 14 (Package.swift). Each tab
        // carries a `.tag` so the selection binding can address it.
        TabView(selection: $selection) {
            ContentView()
                .tabItem { Label("Verdict", systemImage: "gauge.with.needle") }
                .tag(DetailTab.verdict)
            HistoryView()
                .tabItem { Label("History", systemImage: "chart.xyaxis.line") }
                .tag(DetailTab.history)
            CensusView()
                .tabItem { Label("Census", systemImage: "list.bullet.rectangle") }
                .tag(DetailTab.census)
            AlertsView()
                .tabItem { Label("Alerts", systemImage: "bell") }
                .tag(DetailTab.alerts)
        }
        .environment(detail)
    }
}
