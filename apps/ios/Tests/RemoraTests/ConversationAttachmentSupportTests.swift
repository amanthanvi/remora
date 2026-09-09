import UIKit
import XCTest
@testable import Remora

final class ConversationAttachmentSupportTests: XCTestCase {
    func testBuildTurnInputsOmitsWhitespaceOnlyTextAndKeepsAttachmentInput() {
        let attachment = AppUserInput.image(url: "data:image/png;base64,abc")

        let inputs = ConversationAttachmentSupport.buildTurnInputs(
            text: "   \n",
            additionalInput: [attachment]
        )

        XCTAssertEqual(inputs.count, 1)
        guard case .image(let url)? = inputs.first else {
            return XCTFail("Expected image input")
        }
        XCTAssertEqual(url, "data:image/png;base64,abc")
    }

    func testPreparedAttachmentCreatesImageUserInput() throws {
        let attachment = try XCTUnwrap(
            PreparedImageAttachment(
                data: Data([0x01, 0x02, 0x03]),
                mimeType: "image/png"
            ) as PreparedImageAttachment?
        )

        guard case .image(let url) = attachment.userInput else {
            return XCTFail("Expected image user input")
        }

        XCTAssertEqual(url, "data:image/png;base64,AQID")
    }

    @MainActor
    func testPrepareImageUsesPNGWhenImageHasTransparency() async {
        let image = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).image { context in
            UIColor.clear.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
            UIColor.systemGreen.setFill()
            context.fill(CGRect(x: 1, y: 1, width: 2, height: 2))
        }

        let attachment = await ConversationAttachmentSupport.prepareImage(image)

        XCTAssertEqual(attachment?.mimeType, "image/png")
        XCTAssertNotNil(attachment?.data)
    }

    @MainActor
    func testPrepareImageUsesJPEGWhenImageIsOpaque() async {
        let format = UIGraphicsImageRendererFormat()
        format.opaque = true
        let image = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4), format: format).image { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }

        let attachment = await ConversationAttachmentSupport.prepareImage(image)

        XCTAssertEqual(attachment?.mimeType, "image/jpeg")
        XCTAssertNotNil(attachment?.data)
    }

    @MainActor
    func testLargeImagePreparationMeasuresResponsiveness() async throws {
        for opaque in [false, true] {
            let format = UIGraphicsImageRendererFormat()
            format.scale = 1
            format.opaque = opaque
            let size = CGSize(width: 4_096, height: 3_072)
            let image = UIGraphicsImageRenderer(size: size, format: format).image { context in
                for x in stride(from: 0, to: 4_096, by: 8) {
                    UIColor(hue: CGFloat(x % 360) / 360, saturation: 0.8, brightness: 0.9, alpha: 1).setFill()
                    context.fill(CGRect(x: x, y: 0, width: 8, height: 3_072))
                }
            }
            let heartbeat = Task { @MainActor in
                var ticks = 0
                var maximumGap = 0.0
                var previous = ContinuousClock.now
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .milliseconds(5)) }
                    catch { break }
                    let now = ContinuousClock.now
                    let gap = previous.duration(to: now).components
                    maximumGap = max(maximumGap, Double(gap.seconds) + Double(gap.attoseconds) / 1e18)
                    previous = now
                    ticks += 1
                }
                return (ticks, maximumGap)
            }
            await Task.yield()
            let start = ContinuousClock.now
            let attachment = await ConversationAttachmentSupport.prepareImage(image)
            let prepared = try XCTUnwrap(attachment)
            guard case .image(let url) = prepared.userInput else {
                return XCTFail("Expected prepared image input")
            }
            let elapsed = start.duration(to: .now)
            heartbeat.cancel()
            let (ticks, maximumGap) = await heartbeat.value
            print("SEND_IMAGE_PREPARATION image=4096x3072 mime=\(prepared.mimeType) bytes=\(prepared.data.count) data_uri_bytes=\(url.utf8.count) elapsed=\(elapsed) main_actor_ticks=\(ticks) max_tick_gap_seconds=\(maximumGap)")
            XCTAssertEqual(prepared.mimeType, opaque ? "image/jpeg" : "image/png")
            XCTAssertTrue(url.hasPrefix("data:\(prepared.mimeType);base64,"))
        }
    }

    @MainActor
    func testMissingAndInvalidImagesDoNotCreateSendPayloads() async {
        let missing = await ConversationAttachmentSupport.prepareImage(nil)
        let invalid = await ConversationAttachmentSupport.prepareImage(UIImage())
        XCTAssertNil(missing)
        XCTAssertNil(invalid)
    }
}
