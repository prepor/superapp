//! The shared reader typography and image template.

use super::pictures::HtmlImage;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    /** An image in a letter. It fits the column and shows muted alternative
        text until the picture lands, or when it cannot. Its bytes come from
        `widgets::pictures`, never from the frame that draws it. */
    mod.widgets.ReaderImage = set_type_default() do #(HtmlImage::register_widget(vm)) {
        width: Fit, height: Fit
        max_width: 360
        max_height: 320
        image: mod.widgets.Image { width: Fill, height: Fill }
        draw_text +: {
            text_style: mod.widgets.SProseStyle{}
            color: #909090
        }
    }

    /** An HTML letter in the app's own face and colours. What reaches this
        widget has been through `reader::html`: no fetching element survives,
        link hrefs carry only http, https and mailto, and everything is
        escaped. */
    mod.widgets.ReaderHtml = Html {
        width: Fill, height: Fit
        padding: 0
        margin: 0
        // A letter is read, so it is selectable.
        selectable: true

        font_size: 11.25
        font_color: #141414
        draw_text +: { color: #141414 }

        text_style_normal: mod.widgets.SProseStyle{}
        text_style_italic: mod.widgets.SProseItalicStyle{}
        text_style_bold: mod.widgets.SProseBoldStyle{}
        text_style_bold_italic: mod.widgets.SProseBoldItalicStyle{}
        text_style_fixed: mod.widgets.SMonoStyle{line_spacing: 1.2}

        // The reader uses a compact heading scale (see reader::set_html).
        // Margins are in ems; the other insets are logical pixels.
        heading_margin: Inset{top: 1.0, bottom: 0.3}
        paragraph_margin: Inset{top: 0.55, bottom: 0.55}

        // Code stays mono, slightly smaller to match the prose's x-height.
        // Horizontal insets distinguish it without inflating the whole line.
        fixed_font_size_scale: 0.9
        inline_code_padding: Inset{left: 2, right: 2, top: 0, bottom: 0}
        inline_code_margin: 0
        code_layout +: {
            padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        }
        quote_layout +: {
            padding: Inset{left: 12, right: 10, top: 8, bottom: 8}
        }
        list_item_layout +: {
            padding: Inset{left: 6, right: 0, top: 5, bottom: 5}
        }
        // The separator shader leaves one pixel above and below its rule.
        sep_walk +: {
            height: 3
            margin: Inset{top: 6, bottom: 6}
        }

        // Different marks, so a nested list is easy to scan.
        ul_markers: ["•", "-"]
        ol_separator: "."

        a := mod.widgets.HtmlLink {
            color: #5a5a5a
            pressed_color: #141414
        }
        img := mod.widgets.ReaderImage {}

        // The selection stays above the panel background.
        draw_selection +: {
            draw_call_group: @selection
            color: #00000020
        }

        draw_block +: {
            line_color: #141414
            sep_color: #dcdcdc
            quote_bg_color: #fafafa
            quote_fg_color: #dcdcdc
            space_1: uniform(2.0)
            space_2: uniform(4.0)
            code_color: #f4f4f4
            table_border_color: #dcdcdc
            // A background here would cover the header text; bold already
            // tells a header cell apart.
            table_header_bg_color: #0000
            selection_color: #00000020
        }
    }

}
