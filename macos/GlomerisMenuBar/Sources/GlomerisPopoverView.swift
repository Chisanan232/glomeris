//
//  GlomerisPopoverView.swift
//  GlomerisMenuBar
//
//  Empty popover content for the menu-bar item. HORO-1059 scope is limited
//  to "renders and opens" — no data, no candidate list, no Process spawning.
//  See the standing project rule in GlomerisMenuBarApp.swift before adding
//  anything here beyond presentation.
//

import SwiftUI

struct GlomerisPopoverView: View {
    var body: some View {
        VStack {
            Text("Glomeris")
                .font(.headline)
            Text("Menu-bar skeleton — no functionality yet.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(width: 240, height: 120)
    }
}

#Preview {
    GlomerisPopoverView()
}
