//
//  GlomerisPopoverView.swift
//  GlomerisMenuBar
//
//  HORO-1059 introduced this as an empty popover skeleton; HORO-1062 adds
//  the status + daemon-health section (StatusHealthSectionView); HORO-1063
//  adds the candidates list section (CandidatesSectionView). See the
//  standing project rule in GlomerisMenuBarApp.swift before adding
//  anything here beyond presentation.
//

import SwiftUI

struct GlomerisPopoverView: View {
    var body: some View {
        VStack(alignment: .leading) {
            Text("Glomeris")
                .font(.title3)
            StatusHealthSectionView()
            Divider()
            CandidatesSectionView()
        }
        .padding()
        .frame(width: 260)
    }
}

#Preview {
    GlomerisPopoverView()
}
