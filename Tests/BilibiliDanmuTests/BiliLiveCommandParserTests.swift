import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili live command parser")
struct BiliLiveCommandParserTests {
    @Test func danmuQuestionTextRemainsAStandardDanmuEvent() {
        let raw: [String: Any] = [
            "cmd": "DANMU_MSG:4:0:2:2:2:0",
            "info": [
                [],
                "为什么数据架构还要学 Flink？",
                [12345, "石头"],
                [7, "拾穗数据"],
                [31],
            ],
        ]

        let event = BiliLiveCommandParser.parse(
            rawCommand: raw,
            timestamp: Date(timeIntervalSince1970: 1),
            id: "danmu-1"
        )

        #expect(event == DanmuEvent(
            id: "danmu-1",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 1),
            username: "石头",
            authorID: "12345",
            content: "为什么数据架构还要学 Flink？"
        ))
    }
    @Test func danmuEmoteMetadataBecomesStandardInlineEmotes() throws {
        let raw: [String: Any] = [
            "cmd": "DANMU_MSG",
            "info": [
                [
                    0, 1, 25, 16_777_215, 1_700_000_000, 0, 0, "hash",
                    0, 0, 0, "", 0, [:], nil,
                    [
                        "extra": [
                            "emots": [
                                "[热]": [
                                    "emoji": "[热]",
                                    "url": "http://i0.hdslb.com/bfs/live/heat.png",
                                    "width": 20,
                                    "height": 20,
                                    "is_dynamic": 0,
                                ],
                                "[主播专属]": [
                                    "descript": "[主播专属]",
                                    "url": "https://i1.hdslb.com/bfs/live/room-emote.gif",
                                    "width": 162,
                                    "height": 162,
                                    "is_dynamic": 1,
                                ],
                            ],
                        ],
                    ],
                ],
                "一起[热][主播专属]",
                [12345, "表情观众"],
            ],
        ]

        let event = try #require(BiliLiveCommandParser.parse(rawCommand: raw, id: "emote-danmu"))
        let heat = try #require(event.emotes.first { $0.text == "[热]" })
        let roomEmote = try #require(event.emotes.first { $0.text == "[主播专属]" })

        #expect(event.content == "一起[热][主播专属]")
        #expect(event.emotes.count == 2)
        #expect(heat.imageURL.absoluteString == "https://i0.hdslb.com/bfs/live/heat.png")
        #expect(heat.width == 20)
        #expect(heat.height == 20)
        #expect(!heat.isAnimated)
        #expect(roomEmote.isAnimated)
    }

    @Test func emptyEmoteDanmuUsesTheReceivedEmoteTextAsFallback() throws {
        let raw: [String: Any] = [
            "cmd": "DANMU_MSG",
            "info": [
                [
                    0, 1, 25, 16_777_215, 1_700_000_000, 0, 0, "hash",
                    0, 0, 0, "", 0, [:], nil,
                    [
                        "extra": #"{"emots":{"[热]":{"emoji":"[热]","url":"https://i0.hdslb.com/bfs/live/heat.png"}}}"#,
                    ],
                ],
                "",
                [12345, "表情观众"],
            ],
        ]

        let event = try #require(BiliLiveCommandParser.parse(rawCommand: raw, id: "empty-emote"))

        #expect(event.content == "[热]")
        #expect(event.emotes.map(\.text) == ["[热]"])
    }

    @Test func replyDanmuPreservesMentionedUsernameFromModeInfo() {
        let raw: [String: Any] = [
            "cmd": "DANMU_MSG:4:0:2:2:2:0",
            "info": [
                [
                    0, 1, 25, 16_777_215, 1_700_000_000, 0, 0, "hash",
                    0, 0, 0, "", 0, [:], nil,
                    ["extra": #"{"reply_uname":"观众甲"}"#],
                ],
                "这个方案更适合批处理",
                [12345, "观众乙"],
            ],
        ]

        let event = BiliLiveCommandParser.parse(rawCommand: raw, id: "reply-danmu")

        #expect(event?.content == "回复 @观众甲: 这个方案更适合批处理")
    }

    @Test func directReplyMetadataRestoresMentionFromObservedLiveShape() throws {
        let raw: [String: Any] = [
            "cmd": "DANMU_MSG",
            "info": [
                [
                    0, 1, 25, 16_777_215, 1_700_000_000, 0, 0, "hash",
                    0, 0, 0, "", 0, [:], "",
                    [
                        "show_reply": true,
                        "reply_mid": 0,
                        "reply_uname": "WenjunXueCPP",
                        "reply_is_mystery": false,
                    ],
                ],
                " test一下",
                [0, "D***"],
            ],
        ]
        let jsonData = try #require(try? JSONSerialization.data(withJSONObject: raw))

        let event = BiliLiveCommandParser.parse(jsonData: jsonData, id: "observed-reply")

        #expect(event?.content == "回复 @WenjunXueCPP: test一下")
    }

    @Test func giftCommandBecomesStandardGiftEvent() {
        let raw: [String: Any] = [
            "cmd": "SEND_GIFT",
            "data": [
                "uname": "观众甲",
                "uid": 12345,
                "giftName": "小花花",
                "num": 3,
            ],
        ]

        let event = BiliLiveCommandParser.parse(rawCommand: raw, id: "gift-1")

        #expect(event?.kind == .gift)
        #expect(event?.username == "观众甲")
        #expect(event?.authorID == "12345")
        #expect(event?.content == "观众甲 送出 小花花 x3")
    }

    @Test func remainingStandardCommandsBecomeExpectedEventKinds() {
        let superchat: [String: Any] = [
            "cmd": "SUPER_CHAT_MESSAGE",
            "data": ["message": "这个方案怎么迁移？", "user_info": ["uname": "观众乙"]],
        ]
        let guardBuy: [String: Any] = [
            "cmd": "GUARD_BUY",
            "data": ["username": "观众丙", "gift_name": "舰长"],
        ]
        let entry: [String: Any] = [
            "cmd": "ENTRY_EFFECT",
            "data": ["uid": 123, "copy_writing": "<%观众丁%> 来了"],
        ]
        let like: [String: Any] = [
            "cmd": "LIKE_INFO_V3_CLICK",
            "data": ["uname": "观众戊"],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: superchat, id: "sc")?.kind == .superchat)
        #expect(BiliLiveCommandParser.parse(rawCommand: guardBuy, id: "guard")?.kind == .guardEvent)
        #expect(BiliLiveCommandParser.parse(rawCommand: entry, id: "enter")?.kind == .enter)
        #expect(BiliLiveCommandParser.parse(rawCommand: like, id: "like")?.kind == .like)
        #expect(BiliLiveCommandParser.parse(rawCommand: ["cmd": "UNKNOWN"]) == nil)
    }

    @Test(arguments: ["SUPER_CHAT_MESSAGE_JP", "SUPER_CHAT_MESSAGE_JPN"])
    func upstreamSuperchatAliasesUseTheSameMapping(command: String) {
        let raw: [String: Any] = [
            "cmd": command,
            "data": ["message": "这个方案如何迁移？", "user_info": ["uname": "观众"]],
        ]

        let event = BiliLiveCommandParser.parse(rawCommand: raw, id: command)

        #expect(event?.kind == .superchat)
        #expect(event?.content == "这个方案如何迁移？")
    }

    @Test func repeatedRawEventProducesStableIdentifierForSessionDeduplication() {
        let raw: [String: Any] = [
            "cmd": "LIKE_INFO_V3_CLICK",
            "data": ["uname": "观众", "uid": 42, "timestamp": 1_700_000_000],
        ]

        let first = BiliLiveCommandParser.parse(rawCommand: raw)
        let repeated = BiliLiveCommandParser.parse(rawCommand: raw)

        #expect(first?.id == repeated?.id)
    }

    @Test func interactWordDistinguishesEntryFollowAndShare() {
        let entry: [String: Any] = [
            "cmd": "INTERACT_WORD",
            "data": ["uname": "观众甲", "msg_type": 1],
        ]
        let follow: [String: Any] = [
            "cmd": "INTERACT_WORD",
            "data": ["uname": "观众乙", "msg_type": 2],
        ]
        let share: [String: Any] = [
            "cmd": "INTERACT_WORD",
            "data": ["uname": "观众丙", "msg_type": 3],
        ]
        let specialFollow: [String: Any] = [
            "cmd": "INTERACT_WORD",
            "data": ["uname": "观众丁", "msg_type": 4],
        ]
        let mutualFollow: [String: Any] = [
            "cmd": "INTERACT_WORD",
            "data": ["uname": "观众戊", "msg_type": 5],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: entry, id: "entry")?.kind == .enter)
        #expect(BiliLiveCommandParser.parse(rawCommand: follow, id: "follow")?.kind == .follow)
        #expect(BiliLiveCommandParser.parse(rawCommand: follow, id: "follow")?.content == "观众乙 关注了主播")
        #expect(BiliLiveCommandParser.parse(rawCommand: share, id: "share")?.kind == .share)
        #expect(BiliLiveCommandParser.parse(rawCommand: share, id: "share")?.content == "观众丙 分享了直播间")
        #expect(BiliLiveCommandParser.parse(rawCommand: specialFollow, id: "special")?.content == "观众丁 特别关注了主播")
        #expect(BiliLiveCommandParser.parse(rawCommand: mutualFollow, id: "mutual")?.content == "观众戊 与主播互相关注")
    }

    @Test func pkLifecycleCommandsBecomePKEvents() {
        let start: [String: Any] = [
            "cmd": "PK_BATTLE_START_NEW",
            "data": ["pk_id": 9988],
        ]
        let end: [String: Any] = [
            "cmd": "PK_BATTLE_END",
            "data": ["pk_id": 9988],
        ]
        let punish: [String: Any] = [
            "cmd": "PK_BATTLE_VIDEO_PUNISH_BEGIN",
            "data": ["pk_id": 9988],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: start, id: "pk-start")?.kind == .pk)
        #expect(BiliLiveCommandParser.parse(rawCommand: start, id: "pk-start")?.content == "PK 已开始")
        #expect(BiliLiveCommandParser.parse(rawCommand: end, id: "pk-end")?.kind == .pk)
        #expect(BiliLiveCommandParser.parse(rawCommand: end, id: "pk-end")?.content == "PK 已结束")
        #expect(BiliLiveCommandParser.parse(rawCommand: punish, id: "pk-punish")?.content == "PK 惩罚阶段开始")
    }

    @Test func lotteryAndRedPocketCommandsBecomeLotteryEvents() {
        let lottery: [String: Any] = [
            "cmd": "ANCHOR_LOT_START",
            "data": ["award_name": "签名书"],
        ]
        let redPocket: [String: Any] = [
            "cmd": "POPULARITY_RED_POCKET_START",
            "data": ["lot_id": 42],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: lottery, id: "lottery")?.kind == .lottery)
        #expect(BiliLiveCommandParser.parse(rawCommand: lottery, id: "lottery")?.content == "抽奖开始：签名书")
        #expect(BiliLiveCommandParser.parse(rawCommand: redPocket, id: "red-pocket")?.kind == .lottery)
        #expect(BiliLiveCommandParser.parse(rawCommand: redPocket, id: "red-pocket")?.content == "红包活动开始")
    }

    @Test func roomAdminAndMuteCommandsBecomeModerationEvents() {
        let admin: [String: Any] = [
            "cmd": "ROOM_ADMIN_ENTRANCE",
            "data": ["uname": "观众甲"],
        ]
        let muted: [String: Any] = [
            "cmd": "ROOM_BLOCK_MSG",
            "data": ["uname": "观众乙"],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: admin, id: "admin")?.kind == .moderation)
        #expect(BiliLiveCommandParser.parse(rawCommand: admin, id: "admin")?.content == "观众甲 成为房管")
        #expect(BiliLiveCommandParser.parse(rawCommand: muted, id: "muted")?.kind == .moderation)
        #expect(BiliLiveCommandParser.parse(rawCommand: muted, id: "muted")?.content == "观众乙 被禁言")
    }

    @Test func liveAndPreparingCommandsBecomeRoomStatusEvents() {
        let live: [String: Any] = ["cmd": "LIVE", "roomid": 123]
        let preparing: [String: Any] = ["cmd": "PREPARING", "roomid": 123]

        #expect(BiliLiveCommandParser.parse(rawCommand: live, id: "live")?.kind == .roomStatus)
        #expect(BiliLiveCommandParser.parse(rawCommand: live, id: "live")?.content == "直播已开始")
        #expect(BiliLiveCommandParser.parse(rawCommand: preparing, id: "preparing")?.kind == .roomStatus)
        #expect(BiliLiveCommandParser.parse(rawCommand: preparing, id: "preparing")?.content == "直播已结束")
    }

    @Test func platformNoticesBecomeSanitizedSystemEvents() {
        let warning: [String: Any] = [
            "cmd": "WARNING",
            "data": ["msg": "请遵守直播规范"],
        ]
        let commonNotice: [String: Any] = [
            "cmd": "COMMON_NOTICE_DANMAKU",
            "data": [
                "content_segments": [
                    ["text": "本场活动"],
                    ["text": "即将开始"],
                ],
            ],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: warning, id: "warning")?.kind == .system)
        #expect(BiliLiveCommandParser.parse(rawCommand: warning, id: "warning")?.content == "请遵守直播规范")
        #expect(BiliLiveCommandParser.parse(rawCommand: commonNotice, id: "notice")?.kind == .system)
        #expect(BiliLiveCommandParser.parse(rawCommand: commonNotice, id: "notice")?.content == "本场活动即将开始")
    }

    @Test func crossRoomGiftPromotionIsIgnored() {
        let notice: [String: Any] = [
            "cmd": "NOTICE_MSG",
            "data": [
                "msg_common": "<%bili_5827586016%>投喂<%有狐天天--%>1个浪漫城堡，点击前往TA的房间吧！",
                "msg_type": 2,
                "roomid": 582758601,
            ],
        ]

        #expect(BiliLiveCommandParser.parse(rawCommand: notice, id: "promotion") == nil)
    }
}
