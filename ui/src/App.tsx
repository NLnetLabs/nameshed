import axios from 'axios'
import react from 'react'
import './App.css'

const API_BASE_URL = 'http://rusty.nlnetlabs.nl:8080/'

import { RendererSvg } from '@msagl/renderer-svg'
import { Node, Graph, Edge } from '@msagl/core'
import { DrawingEdge, Color, StyleEnum, ShapeEnum } from '@msagl/drawing'
import { DrawingNode } from '@msagl/drawing'

interface ZoneSignerStatusResponse {
  zones_being_signed: ZoneSigningReport[]
}

type ZoneReports = Map<string, ZoneReport>

interface ZoneReport {
  role: Role,
}

interface Role {
  Primary: Primary,
  Secondary: Secondary,
}

type Primary = string

interface LoadingStatus {
  RefreshInProgress: number
}

interface Secondary {
  status: string | LoadingStatus,
  last_refresh_checked_secs_ago: number,
  last_refresh_checked_serial: number
  last_refresh_succeeded_secs_ago: number,
  last_refresh_succeeded_serial: number
  next_refresh_secs_from_now: number,
  next_refresh_cause: string,
}

interface InProgress {
  denial_rr_count: number,
  denial_time: number,
  insertion_time: number,
  requested_at: number,
  rrsig_count: number,
  rrsig_reused_count: number,
  rrsig_time: number,
  sort_time: number,
  started_at: number,
  threads_used: number,
  total_time: number,
  unsigned_rr_count: number,
  walk_time: number,
  zone_serial: number,
}

interface Finished {
  denial_rr_count: number,
  denial_time: number,
  finished_at: number,
  insertion_time: number,
  requested_at: number,
  rrsig_count: number,
  rrsig_reused_count: number,
  rrsig_time: number,
  sort_time: number,
  started_at: number,
  threads_used: number,
  total_time: number,
  unsigned_rr_count: number,
  walk_time: number,
  zone_serial: number,
}

interface SigningStatus {
  InProgress: InProgress
  Finished: Finished
}

interface ZoneSigningReport {
  zone_name: string,
  status: SigningStatus,
}

function createGraph(zone_reports: ZoneReports, zone_signing_report: ZoneSigningReport[]): Graph {
  const graph = new Graph()
  const nodes_by_name = new Map();

  // add a root node.
  const subGraph = new Graph('._sub')
  const sub_d = new DrawingNode(subGraph)
  sub_d.labelText = ''
  sub_d.penwidth = 0.1
  sub_d.fontsize = 4
  // sub_d.LabelMargin = 5
  sub_d.color = Color.Black
  let node_title = '.'
  const root = new Node(node_title)
  nodes_by_name.set(node_title, root)
  const root_d = new DrawingNode(root)
  root_d.shape = ShapeEnum.plaintext
  subGraph.addNode(root)
  graph.addNode(subGraph)
  nodes_by_name.set('._sub', subGraph)

  const zone_names = Array.from(zone_reports.keys()).sort()
  // add nodes below 
  for (const zone_name of zone_names) {
    let zone_report = zone_reports.get(zone_name)
    if (zone_report === undefined) {
      continue
    }

    let abs_zone_name: string = zone_name + '.'

    // add a zone node
    node_title = abs_zone_name
    const subGraph = new Graph(abs_zone_name + '_sub')
    const sub_d = new DrawingNode(subGraph)
    sub_d.labelText = ''
    sub_d.penwidth = 0.1
    sub_d.fontsize = 4
    // sub_d.LabelMargin = 5
    sub_d.color = Color.Black

    const node = new Node(node_title)
    nodes_by_name.set(node_title, node)
    subGraph.addNode(node)
    const node_d = new DrawingNode(node)
    node_d.shape = ShapeEnum.plaintext

    const box = new Node(node_title + '_details')
    nodes_by_name.set(node_title, box)
    const box_d = new DrawingNode(box)
    if (zone_report.role.Primary !== undefined) {
      box_d.labelText = 'Primary'
    } else if (zone_report.role.Secondary !== undefined) {
      let loading_status = zone_report.role.Secondary.status
      console.log(loading_status)
      if (loading_status.RefreshInProgress !== undefined) {
        loading_status = 'Receiving (' + loading_status.RefreshInProgress + ' updates applied)'
      }
      let status = zone_signing_report.find(report => report.zone_name == zone_name )?.status
      let sign_status = ' '
      if (status !== undefined) {
        if (status.Finished !== undefined) {
          sign_status = ' [Signed] '
        } else if (status.InProgress !== undefined) {
          sign_status = ' [Signing] '
        }
      }
      if (zone_report.role.Secondary.last_refresh_succeeded_serial !== null) {
        box_d.labelText = 'Secondary: ' + zone_report.role.Secondary.last_refresh_succeeded_serial + sign_status + '\n' + loading_status + ' (' + zone_report.role.Secondary.next_refresh_secs_from_now + ' secs away due to ' + zone_report.role.Secondary.next_refresh_cause + ')\n'
      } else {
        box_d.labelText = 'Secondary: ' + loading_status + '\n'
      }
      // box_d.labelText = 'Secondary: ' + zone_report.role.Secondary.status + '\n'
      // box_d.labelText += 'Last refresh succeeded ' + zone_report.role.Secondary.last_refresh_succeeded_secs_ago + ' seconds ago with serial ' + zone_report.role.Secondary.last_refresh_succeeded_serial + '\n'
      // box_d.labelText += 'Last SOA check succeeded ' + zone_report.role.Secondary.last_refresh_checked_secs_ago + ' seconds ago with serial ' + zone_report.role.Secondary.last_refresh_checked_serial + '\n'
      // box_d.labelText += 'Next refresh ' + zone_report.role.Secondary.next_refresh_secs_from_now + ' seconds from now (due to ' + zone_report.role.Secondary.next_refresh_cause + ')\n'
      // let status = zone_signing_report.find(report => report.zone_name == zone_name )?.status
      // if (status !== undefined) {
      //   if (status.Finished !== undefined) {
      //     box_d.labelText += 'Last signed ' + status.Finished.finished_at + ' seconds ago in ' + status.Finished.total_time + ' seconds with serial ' + status.Finished.zone_serial
      //   } else if (status.InProgress !== undefined) {
      //     box_d.labelText += 'Signing since ' + status.InProgress.started_at + ' seconds ago with serial ' + status.InProgress.zone_serial + '\n'
      //     box_d.labelText += '(walk time ' + status.InProgress.walk_time;
      //     box_d.labelText += ', sort time ' + status.InProgress.sort_time;
      //     box_d.labelText += ', denial time ' + status.InProgress.denial_time
      //     box_d.labelText += ', rrsig time ' + status.InProgress.rrsig_time
      //     box_d.labelText += ', insertion time ' + status.InProgress.insertion_time
      //     box_d.labelText += ')'
      //   }
      // }
    } else {
      box_d.labelText = ''
    }
    box_d.penwidth = 0
    box_d.fontsize = 6
    // box_d.LabelMargin = 5
    box_d.color = Color.Black
    subGraph.addNode(box)

    nodes_by_name.set(abs_zone_name + '_sub', subGraph)
    graph.addNode(subGraph)

    // get the parent zone node
    let abs_zone_name_labels = abs_zone_name.split('.')
    let parent_zone_name = abs_zone_name_labels.slice(1).join('.')
    if (parent_zone_name == '') {
      parent_zone_name = '.'
    }
    let parent_zone_node = nodes_by_name.get(parent_zone_name + '_sub')
    let this_zone_node = nodes_by_name.get(abs_zone_name + '_sub')

    // draw an edge from child to parent
    const edge = new Edge(parent_zone_node, this_zone_node)
    const edge_d = new DrawingEdge(edge, true)
    edge_d.color = Color.Black
    edge_d.penwidth = 0.1
    edge_d.styles.push(StyleEnum.solid)
  }

  return graph
}

function App() {
  const [zone_reports, setZoneReports] = react.useState<ZoneReports>()
  const [zone_signing_reports, setSigningReports] = react.useState<ZoneSigningReport[]>()

  react.useEffect(() => {
    axios.get(API_BASE_URL + 'zl/status.json').then((response) => {
      setZoneReports(new Map<string, ZoneReport>(Object.entries(response.data)))

      axios.get<ZoneSignerStatusResponse>(API_BASE_URL + 'zs/status.json').then((response) => {
        setSigningReports(response.data.zones_being_signed)
      });
    });
  }, []);

  if (zone_reports && zone_reports && zone_signing_reports) {
    const viewer = document.getElementById('viewer') || undefined
    const svgRenderer = new RendererSvg(viewer)
    svgRenderer.layoutEditingEnabled = false
    const graph = createGraph(zone_reports, zone_signing_reports)
    svgRenderer.setGraph(graph)
  }

  return (
    <>
    </>
  )
}

export default App
