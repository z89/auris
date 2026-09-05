import QtQuick
import qs.Common
import qs.Widgets
import qs.Modules.Plugins

PluginSettings {
    id: root

    pluginId: "auris"

    StyledText {
        width: parent.width
        text: "AirPods"
        font.pixelSize: Theme.fontSizeLarge
        font.weight: Font.Bold
        color: Theme.surfaceText
    }

    StyledText {
        width: parent.width
        text: "Battery, ear detection and noise control come from the aurisd daemon, which writes $XDG_RUNTIME_DIR/aurisd/state.json. Left click the bar pill for the panel, right click to flip between ANC and Transparency."
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.surfaceVariantText
        wrapMode: Text.WordWrap
    }

    ToggleSetting {
        settingKey: "showPercent"
        label: "Show percentage"
        description: "Print the battery level next to the icon on the bar"
        defaultValue: true
    }

    SelectionSetting {
        settingKey: "pillValue"
        label: "Bar value"
        description: "Which battery the pill reports"
        options: [
            {
                "label": "Lowest bud",
                "value": "buds"
            },
            {
                "label": "Lowest of all three",
                "value": "all"
            },
            {
                "label": "Left bud",
                "value": "left"
            },
            {
                "label": "Right bud",
                "value": "right"
            },
            {
                "label": "Case",
                "value": "case"
            }
        ]
        defaultValue: "buds"
    }

    SliderSetting {
        settingKey: "lowThreshold"
        label: "Low battery"
        description: "Levels at or below this turn amber"
        defaultValue: 20
        minimum: 5
        maximum: 50
        unit: "%"
        leftIcon: "battery_alert"
    }

    SliderSetting {
        settingKey: "criticalThreshold"
        label: "Critical battery"
        description: "Levels at or below this turn red"
        defaultValue: 10
        minimum: 1
        maximum: 25
        unit: "%"
        leftIcon: "battery_alert"
    }

    ToggleSetting {
        settingKey: "hideWhenDisconnected"
        label: "Hide when disconnected"
        description: "Drop the pill from the bar while the AirPods are not connected; it reappears on its own when they connect"
        defaultValue: true
    }

    ToggleSetting {
        settingKey: "popupOnConnect"
        label: "Show stats when AirPods connect"
        description: "Open the battery popout for a few seconds each time the AirPods connect"
        defaultValue: true
    }

    SliderSetting {
        settingKey: "popupSeconds"
        label: "Popup duration"
        description: "How long the connect popout stays open"
        defaultValue: 6
        minimum: 2
        maximum: 20
        unit: "s"
    }
}
